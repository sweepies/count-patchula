//! Local webhook replay against a running server.

use crate::{Args, Result};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpStream, ToSocketAddrs},
};

/// Signs a saved webhook body with the secret the server reads, and posts it
/// to a loopback server. Succeeds when the server accepts it.
pub fn replay(args: Args) -> Result {
    let args = args.positional(3, 4)?;
    let (payload, event, delivery) = (&args[0], &args[1], &args[2]);
    let url = args.get(3).map_or("http://127.0.0.1:8080/webhook", String::as_str);
    let rest = url.strip_prefix("http://").ok_or("replay target must be an http:// URL")?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, port.parse()?),
        _ => (authority, 80u16),
    };
    if !["127.0.0.1", "localhost", "[::1]"].contains(&host) {
        return Err("replay target must be loopback".into());
    }
    let secret = match (
        std::env::var("PATCHULA_WEBHOOK_SECRET_FILE").ok(),
        std::env::var("PATCHULA_WEBHOOK_SECRET").ok(),
    ) {
        (Some(_), Some(_)) => return Err("choose a secret file or environment value".into()),
        (Some(file), None) => std::fs::read_to_string(file)?,
        (None, value) => value.unwrap_or_default(),
    };
    let secret = secret.trim_end_matches(['\r', '\n']);
    if secret.is_empty() {
        return Err("set PATCHULA_WEBHOOK_SECRET_FILE or PATCHULA_WEBHOOK_SECRET".into());
    }
    let body = std::fs::read(payload)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(&body);
    let signature: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let address = (host.trim_start_matches('[').trim_end_matches(']'), port)
        .to_socket_addrs()?
        .next()
        .ok_or("the replay target resolves to no address")?;
    let mut stream = TcpStream::connect(address)?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nhost: {authority}\r\ncontent-type: application/json\r\nx-hub-signature-256: sha256={signature}\r\nx-github-event: {event}\r\nx-github-delivery: {delivery}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    let mut status_line = String::new();
    BufReader::new(stream).read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("the server sent no HTTP status")?
        .parse()?;
    println!("HTTP {status}");
    let expected = if event == "ping" { 200 } else { 202 };
    if status != expected {
        return Err(format!("expected HTTP {expected}").into());
    }
    Ok(())
}
