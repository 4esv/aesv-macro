//! Message submission over SMTP with the mailbox's own credential.

use lettre::address::Envelope;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use mailparse::{MailAddr, MailHeaderMap};

use super::{Credential, Security, Server};
use crate::domain::models::{EmailApiError, SubmissionOutcome};

/// What travels: the envelope and the source with its `Bcc` header removed,
/// so blind-copied addresses reach their servers but no recipient sees them.
pub(super) struct Outgoing {
    pub envelope: Envelope,
    pub source: Vec<u8>,
}

fn rejected(message: &str) -> EmailApiError {
    EmailApiError::SendRejected {
        message: message.into(),
    }
}

fn addresses(
    mail: &mailparse::ParsedMail<'_>,
    header: &str,
) -> Result<Vec<Address>, EmailApiError> {
    let mut found = Vec::new();
    for header in mail.headers.get_all_headers(header) {
        let list = mailparse::addrparse_header(header)
            .map_err(|_| rejected("a recipient address does not parse"))?;
        for address in list.iter() {
            let singles = match address {
                MailAddr::Single(single) => vec![single.clone()],
                MailAddr::Group(group) => group.addrs.clone(),
            };
            for single in singles {
                found.push(
                    single
                        .addr
                        .parse::<Address>()
                        .map_err(|_| rejected("a recipient address is not valid"))?,
                );
            }
        }
    }
    Ok(found)
}

/// Read the envelope from a draft's headers and drop its `Bcc` lines.
pub(super) fn outgoing(raw: &[u8]) -> Result<Outgoing, EmailApiError> {
    let mail = mailparse::parse_mail(raw).map_err(|_| rejected("the draft does not parse"))?;
    let from = addresses(&mail, "From")?.into_iter().next();
    let mut recipients = addresses(&mail, "To")?;
    recipients.extend(addresses(&mail, "Cc")?);
    recipients.extend(addresses(&mail, "Bcc")?);
    let envelope =
        Envelope::new(from, recipients).map_err(|_| rejected("the draft has no recipient"))?;

    // Headers end at the first blank line. A Bcc header's folded lines start
    // with whitespace, so they go with it.
    let body_start = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 2)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 1))
        .unwrap_or(raw.len());
    let (headers, body) = raw.split_at(body_start);
    let mut source = Vec::with_capacity(raw.len());
    let mut in_bcc = false;
    for line in headers.split_inclusive(|&byte| byte == b'\n') {
        let continued = line
            .first()
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t');
        if !continued {
            in_bcc = line.len() >= 4 && line[..4].eq_ignore_ascii_case(b"bcc:");
        }
        if !in_bcc {
            source.extend_from_slice(line);
        }
    }
    source.extend_from_slice(body);
    Ok(Outgoing { envelope, source })
}

/// Submit once. A refusal the server gave before taking the message lets
/// the user edit and retry; anything uncertain is `Unknown`, so the send is
/// never repeated blindly.
pub(super) async fn submit(
    server: &Server,
    credential: &Credential,
    outgoing: &Outgoing,
) -> Result<SubmissionOutcome, EmailApiError> {
    let builder = match server.security {
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&server.host),
        Security::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server.host),
        Security::Plain => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &server.host,
        )),
    }
    .map_err(|_| EmailApiError::Permanent {
        message: "the SMTP server name is not valid".into(),
    })?;
    let transport = builder
        .port(server.port)
        .credentials(Credentials::new(
            credential.username.clone(),
            credential.password.clone(),
        ))
        .build();
    match transport
        .send_raw(&outgoing.envelope, &outgoing.source)
        .await
    {
        Ok(_) => Ok(SubmissionOutcome::Accepted),
        // 535 is a refused login (RFC 4954): the app password was revoked.
        Err(error) if error.is_permanent() => match error.status() {
            Some(code) if code.to_string() == "535" => Err(EmailApiError::AuthRequired),
            _ => Err(rejected("the SMTP server refused the message")),
        },
        Err(_) => Ok(SubmissionOutcome::Unknown),
    }
}
