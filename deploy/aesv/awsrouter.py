"""The `localstack` endpoint for aesv: routes each AWS request to a store that keeps its data.

LocalStack's free edition loses S3 objects and DynamoDB items on every restart,
even with PERSISTENCE=1. This router takes LocalStack's hostname and port and
sends each request by the service named in its SigV4 credential scope:

- s3       -> an S3 server that stores plain files on disk (S3_UPSTREAM)
- dynamodb -> DynamoDB Local, which keeps a database file (DYNAMODB_UPSTREAM)
- the rest -> LocalStack, for SQS and KMS, whose contents are transient (CORE_UPSTREAM)

The S3 server has no bucket notifications, so the router keeps each bucket's
notification configuration itself and sends the S3 event to SQS after a
successful write or delete, in the record format AWS sends.
"""

import datetime
import http.client
import http.server
import json
import os
import re
import secrets
import socketserver
import sys
import threading
import time
import urllib.parse
import uuid
import xml.etree.ElementTree as ET

S3_UPSTREAM = os.environ.get("S3_UPSTREAM", "s3store:8080")
DYNAMODB_UPSTREAM = os.environ.get("DYNAMODB_UPSTREAM", "dynamodb:8000")
CORE_UPSTREAM = os.environ.get("CORE_UPSTREAM", "localstack-core:4566")
STATE = os.environ.get("STATE_FILE", "/state/notifications.json")
REGION = os.environ.get("AWS_REGION", "us-east-1")
NS = "{http://s3.amazonaws.com/doc/2006-03-01/}"
HOP = {"connection", "keep-alive", "proxy-connection", "transfer-encoding", "te", "trailer", "upgrade"}
# NOTE: the local stack sets one allow-all CORS rule on every bucket, and the S3
# server has no CORS, so the router answers for it with that rule.
CORS = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, PUT, POST, DELETE, HEAD",
    "Access-Control-Expose-Headers": "ETag",
    "Access-Control-Max-Age": "3600",
}
SCOPE = re.compile(r"Credential=[^/,\s]+/\d{8}/[^/]+/([^/]+)/aws4_request")

_lock = threading.Lock()


def load_state():
    try:
        with open(STATE) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


notifications = load_state()


def save_state():
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(notifications, f)
    os.replace(tmp, STATE)


def service_of(headers, query):
    """The AWS service a request is for, from its signature or target header."""
    m = SCOPE.search(headers.get("Authorization", ""))
    if m:
        return m.group(1)
    cred = query.get("X-Amz-Credential", [""])[0]
    parts = cred.split("/")
    if len(parts) >= 5:
        return parts[3]
    target = headers.get("X-Amz-Target", "")
    if target.startswith("DynamoDB"):
        return "dynamodb"
    # Every SQS, KMS and DynamoDB client signs; an unsigned request is a public
    # read of a bucket, such as the static files Caddy serves.
    return "s3"


def parse_notification(body):
    """Queue configurations from a PutBucketNotificationConfiguration body."""
    root = ET.fromstring(body)
    configs = []
    for qc in root.iter(NS + "QueueConfiguration"):
        rules = {}
        for rule in qc.iter(NS + "FilterRule"):
            name = (rule.findtext(NS + "Name") or "").lower()
            rules[name] = rule.findtext(NS + "Value") or ""
        configs.append({
            "id": qc.findtext(NS + "Id") or str(uuid.uuid4()),
            "queue": qc.findtext(NS + "Queue"),
            "events": [e.text for e in qc.iter(NS + "Event")],
            "prefix": rules.get("prefix", ""),
            "suffix": rules.get("suffix", ""),
        })
    return configs


def matches(pattern, event_name):
    """`s3:ObjectCreated:*` matches `ObjectCreated:Put`."""
    pattern = pattern.removeprefix("s3:")
    if pattern.endswith(":*"):
        return event_name.startswith(pattern[:-1])
    return pattern == event_name


def queue_url(arn):
    # arn:aws:sqs:region:account:name
    _, _, _, _, account, name = arn.split(":", 5)
    return f"http://{CORE_UPSTREAM}/{account}/{name}"


def record(bucket, key, event_name, size, etag, config_id):
    now = datetime.datetime.now(datetime.timezone.utc)
    return {
        "eventVersion": "2.1",
        "eventSource": "aws:s3",
        "awsRegion": REGION,
        "eventTime": now.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z",
        "eventName": event_name,
        "userIdentity": {"principalId": "AWS:aesv"},
        "requestParameters": {"sourceIPAddress": "127.0.0.1"},
        "responseElements": {"x-amz-request-id": secrets.token_hex(8).upper(), "x-amz-id-2": secrets.token_hex(16)},
        "s3": {
            "s3SchemaVersion": "1.0",
            "configurationId": config_id,
            "bucket": {"name": bucket, "ownerIdentity": {"principalId": "aesv"}, "arn": f"arn:aws:s3:::{bucket}"},
            "object": {
                "key": urllib.parse.quote(key),
                "size": size,
                "eTag": etag.strip('"'),
                "sequencer": format(time.time_ns(), "X"),
            },
        },
    }


def emit(bucket, key, event_name, size, etag):
    for cfg in notifications.get(bucket, []):
        if not key.startswith(cfg["prefix"]) or not key.endswith(cfg["suffix"]):
            continue
        if not any(matches(p, event_name) for p in cfg["events"]):
            continue
        body = json.dumps({"Records": [record(bucket, key, event_name, size, etag, cfg["id"])]})
        form = urllib.parse.urlencode({
            "Action": "SendMessage", "Version": "2012-11-05",
            "QueueUrl": queue_url(cfg["queue"]), "MessageBody": body,
        })
        host, port = CORE_UPSTREAM.split(":")
        conn = http.client.HTTPConnection(host, int(port), timeout=10)
        # LocalStack reads the service from the credential scope; it does not
        # check the signature.
        auth = f"AWS4-HMAC-SHA256 Credential=test/20260101/{REGION}/sqs/aws4_request, SignedHeaders=host, Signature=0"
        try:
            conn.request("POST", "/", form, {"Content-Type": "application/x-www-form-urlencoded", "Authorization": auth})
            resp = conn.getresponse()
            resp.read()
            if resp.status >= 300:
                print(f"event to {cfg['queue']} failed: {resp.status}", file=sys.stderr, flush=True)
        except OSError as e:
            # NOTE: a lost event leaves the object stored but unprocessed; the
            # write itself already succeeded, so the client is not failed.
            print(f"event to {cfg['queue']} failed: {e}", file=sys.stderr, flush=True)
        finally:
            conn.close()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def read_body(self):
        if "chunked" in self.headers.get("Transfer-Encoding", "").lower():
            chunks = []
            while True:
                size = int(self.rfile.readline().split(b";")[0].strip() or b"0", 16)
                if size == 0:
                    while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                        pass
                    break
                chunks.append(self.rfile.read(size))
                self.rfile.readline()
            return b"".join(chunks)
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def reply(self, status, body=b"", content_type="application/xml", extra=None):
        self.send_response(status)
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def handle_any(self):
        parsed = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
        body = self.read_body()
        service = service_of(self.headers, query)
        if parsed.path.startswith(("/_localstack", "/_aws")):
            service = "core"

        if service == "s3":
            parts = parsed.path.lstrip("/").split("/", 1)
            bucket = urllib.parse.unquote(parts[0]) if parts[0] else ""
            key = urllib.parse.unquote(parts[1]) if len(parts) > 1 else ""
            if bucket and not key and "notification" in query:
                return self.notification(bucket, body)
            if self.command == "OPTIONS":
                return self.reply(200, extra={
                    **CORS,
                    "Access-Control-Allow-Headers": self.headers.get("Access-Control-Request-Headers", "*"),
                })
            if bucket and not key and "cors" in query:
                return self.reply(200 if self.command == "PUT" else 204 if self.command == "DELETE" else 200)
            status = self.forward(S3_UPSTREAM, body, extra=CORS)
            if 200 <= status < 300 and bucket and key:
                self.after_write(bucket, key, query, body)
            return
        if service == "dynamodb":
            self.forward(DYNAMODB_UPSTREAM, body)
            return
        self.forward(CORE_UPSTREAM, body)

    def notification(self, bucket, body):
        if self.command == "PUT":
            with _lock:
                notifications[bucket] = parse_notification(body) if body.strip() else []
                save_state()
            return self.reply(200)
        configs = notifications.get(bucket, [])
        xml = ['<NotificationConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">']
        for c in configs:
            xml.append(f"<QueueConfiguration><Id>{c['id']}</Id><Queue>{c['queue']}</Queue>")
            xml += [f"<Event>{e}</Event>" for e in c["events"]]
            xml.append("</QueueConfiguration>")
        xml.append("</NotificationConfiguration>")
        return self.reply(200, "".join(xml).encode())

    def after_write(self, bucket, key, query, body):
        etag = self._last_headers.get("etag", "")
        if self.command == "PUT" and "partNumber" not in query and "uploadId" not in query:
            copy = "X-Amz-Copy-Source" in self.headers
            size = int(self.headers.get("X-Amz-Decoded-Content-Length") or len(body))
            emit(bucket, key, "ObjectCreated:Copy" if copy else "ObjectCreated:Put", size, etag)
        elif self.command == "POST" and "uploadId" in query:
            emit(bucket, key, "ObjectCreated:CompleteMultipartUpload", 0, etag)
        elif self.command == "DELETE" and "uploadId" not in query:
            emit(bucket, key, "ObjectRemoved:Delete", 0, "")

    def forward(self, upstream, body, extra=None):
        host, port = upstream.split(":")
        conn = http.client.HTTPConnection(host, int(port), timeout=300)
        # The body is re-sent whole, so its length replaces the client's in any case.
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP and k.lower() != "content-length"}
        headers["Content-Length"] = str(len(body))
        self._last_headers = {}
        try:
            conn.request(self.command, self.path, body=body, headers=headers)
            resp = conn.getresponse()
            data = resp.read()
        except OSError as e:
            self.reply(502, f"upstream {upstream}: {e}".encode(), "text/plain")
            return 502
        finally:
            conn.close()
        self._last_headers = {k.lower(): v for k, v in resp.getheaders()}
        if resp.status >= 400:
            print(f"{self.command} {self.path} -> {upstream}: {resp.status} {data[:300]!r}", file=sys.stderr, flush=True)
        self.send_response(resp.status, resp.reason)
        for k, v in resp.getheaders():
            # send_response already wrote this server's Server and Date.
            if k.lower() not in HOP and k.lower() not in ("content-length", "server", "date"):
                self.send_header(k, v)
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        return resp.status

    do_GET = do_PUT = do_POST = do_DELETE = do_HEAD = do_PATCH = do_OPTIONS = handle_any


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


if __name__ == "__main__":
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    print(f"awsrouter: s3 -> {S3_UPSTREAM}, dynamodb -> {DYNAMODB_UPSTREAM}, rest -> {CORE_UPSTREAM}", flush=True)
    Server(("0.0.0.0", 4566), Handler).serve_forever()
