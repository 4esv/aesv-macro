"""Checks for awsrouter.py. Run: python3 -m unittest deploy/aesv/test_awsrouter.py"""

import http.client
import http.server
import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
STATE_DIR = tempfile.mkdtemp()


class Fake(http.server.BaseHTTPRequestHandler):
    """An upstream that records each request and answers 200 with an ETag."""

    seen = []

    def log_message(self, *a):
        pass

    def any(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        self.seen.append((self.server.name, self.command, self.path, dict(self.headers), body))
        self.send_response(200)
        self.send_header("ETag", '"abc123"')
        self.send_header("Content-Length", "2")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(b"ok")

    do_GET = do_PUT = do_POST = do_DELETE = do_HEAD = any


def serve(handler, name=None):
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.name = name
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


s3 = serve(Fake, "s3")
dynamo = serve(Fake, "dynamodb")
core = serve(Fake, "core")
os.environ.update({
    "S3_UPSTREAM": f"127.0.0.1:{s3.server_port}",
    "DYNAMODB_UPSTREAM": f"127.0.0.1:{dynamo.server_port}",
    "CORE_UPSTREAM": f"127.0.0.1:{core.server_port}",
    "STATE_FILE": os.path.join(STATE_DIR, "notifications.json"),
})
sys.path.insert(0, HERE)
import awsrouter  # noqa: E402

router = awsrouter.Server(("127.0.0.1", 0), awsrouter.Handler)
threading.Thread(target=router.serve_forever, daemon=True).start()


def sig(service):
    return f"AWS4-HMAC-SHA256 Credential=test/20260117/us-east-1/{service}/aws4_request, SignedHeaders=host, Signature=x"


def call(method, path, body=b"", headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", router.server_port, timeout=5)
    conn.request(method, path, body, headers or {})
    resp = conn.getresponse()
    data = resp.read()
    conn.close()
    return resp.status, data


NOTIFY = b"""<NotificationConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
<QueueConfiguration><Id>up</Id><Queue>arn:aws:sqs:us-east-1:000000000000:uploads</Queue>
<Event>s3:ObjectCreated:*</Event>
<Filter><S3Key><FilterRule><Name>prefix</Name><Value>docs/</Value></FilterRule></S3Key></Filter>
</QueueConfiguration></NotificationConfiguration>"""


class Units(unittest.TestCase):
    def test_service_of(self):
        so = awsrouter.service_of
        self.assertEqual(so({"Authorization": sig("sqs")}, {}), "sqs")
        self.assertEqual(so({}, {"X-Amz-Credential": ["test/20260117/us-east-1/s3/aws4_request"]}), "s3")
        self.assertEqual(so({"X-Amz-Target": "DynamoDB_20120810.GetItem"}, {}), "dynamodb")
        self.assertEqual(so({}, {}), "s3")

    def test_parse_notification(self):
        [c] = awsrouter.parse_notification(NOTIFY)
        self.assertEqual(c["id"], "up")
        self.assertEqual(c["queue"], "arn:aws:sqs:us-east-1:000000000000:uploads")
        self.assertEqual(c["events"], ["s3:ObjectCreated:*"])
        self.assertEqual((c["prefix"], c["suffix"]), ("docs/", ""))

    def test_matches(self):
        self.assertTrue(awsrouter.matches("s3:ObjectCreated:*", "ObjectCreated:Put"))
        self.assertTrue(awsrouter.matches("s3:ObjectCreated:Put", "ObjectCreated:Put"))
        self.assertFalse(awsrouter.matches("s3:ObjectCreated:*", "ObjectRemoved:Delete"))
        self.assertFalse(awsrouter.matches("s3:ObjectCreated:Copy", "ObjectCreated:Put"))

    def test_queue_url(self):
        self.assertTrue(awsrouter.queue_url("arn:aws:sqs:us-east-1:000000000000:q").endswith("/000000000000/q"))


class EndToEnd(unittest.TestCase):
    def setUp(self):
        Fake.seen.clear()

    def test_routes_by_service(self):
        call("POST", "/", b"{}", {"Authorization": sig("dynamodb")})
        call("POST", "/", b"Action=ListQueues", {"Authorization": sig("sqs")})
        call("GET", "/_localstack/health")
        call("GET", "/static-file-storage/a.png")
        self.assertEqual([s[0] for s in Fake.seen], ["dynamodb", "core", "core", "s3"])

    def test_put_sends_an_s3_event_to_sqs(self):
        self.assertEqual(call("PUT", "/bucket?notification", NOTIFY, {"Authorization": sig("s3")})[0], 200)
        status, xml = call("GET", "/bucket?notification", b"", {"Authorization": sig("s3")})
        self.assertIn(b"<Queue>arn:aws:sqs:us-east-1:000000000000:uploads</Queue>", xml)
        with open(os.environ["STATE_FILE"]) as f:
            self.assertIn("bucket", json.load(f))

        call("PUT", "/bucket/docs/a%20b.txt", b"hello", {"Authorization": sig("s3")})
        call("PUT", "/bucket/other/c.txt", b"x", {"Authorization": sig("s3")})
        call("PUT", "/bucket/docs/part?partNumber=1&uploadId=u", b"x", {"Authorization": sig("s3")})

        sends = [s for s in Fake.seen if s[0] == "core"]
        self.assertEqual(len(sends), 1, "only the matching whole-object PUT sends an event")
        _, _, _, headers, body = sends[0]
        self.assertIn("/sqs/aws4_request", headers["Authorization"])
        form = urllib.parse.parse_qs(body.decode())
        self.assertEqual(form["Action"], ["SendMessage"])
        self.assertTrue(form["QueueUrl"][0].endswith("/000000000000/uploads"))
        [rec] = json.loads(form["MessageBody"][0])["Records"]
        self.assertEqual(rec["eventName"], "ObjectCreated:Put")
        self.assertEqual(rec["s3"]["bucket"]["name"], "bucket")
        self.assertEqual(rec["s3"]["object"]["key"], "docs/a%20b.txt")
        self.assertEqual(rec["s3"]["object"]["size"], 5)
        self.assertEqual(rec["s3"]["object"]["eTag"], "abc123")
        self.assertEqual(rec["s3"]["configurationId"], "up")


if __name__ == "__main__":
    unittest.main()
