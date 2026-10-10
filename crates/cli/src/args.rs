//! `tinystore serve`'s arguments, as the SDKs start it:
//!
//! ```text
//! tinystore serve --dir <dir> --local --log <dir>/server/serve.log [--idle 30000ms]   a sidecar
//! tinystore serve --dir <dir> --stdio [--clock 2026-10-09T12:00:00.000Z]            a private child
//! tinystore serve <dir> [--durability os] [--keep-free 10GiB]                      a person's server
//! tinystore serve <dir> --encryption-key-file /run/secrets/tinystore.key              its key kept apart from its data
//! tinystore serve <dir> --listen tls://0.0.0.0:7443 --tls-cert c.pem --tls-key k.pem --tokens tokens
//! ```

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tinystore::Durability;

use crate::remote::Address;

/// How long a sidecar stays once its last client has gone, unless `--idle` says.
const SIDECAR_IDLE: Duration = Duration::from_secs(30);
/// The memory a remote server's engines hold at most, unless `--memory` says.
const REMOTE_MEMORY: u64 = 1 << 30;

#[derive(Debug, PartialEq)]
pub(crate) struct Serve {
    pub(crate) dir: PathBuf,
    pub(crate) transport: Transport,
    pub(crate) log: Option<PathBuf>,
    /// How long the server stays with no client; for ever when `None`.
    pub(crate) idle: Option<Duration>,
    /// Where a private server's test clock starts; the system's clock when `None`.
    pub(crate) clock: Option<SystemTime>,
    /// Clients on other machines, beside the local ones.
    pub(crate) remote: Option<Remote>,
    /// The bytes the engines' work may hold at once.
    pub(crate) memory: Option<u64>,
    /// How far a commit of the store's files goes; each engine's own when `None`.
    pub(crate) durability: Option<Durability>,
    /// The disk the store keeps free; the store's own 1 GiB when `None`.
    pub(crate) keep_free: Option<u64>,
    /// The file of the store's encryption key; `encryption.key` of its directory,
    /// which the store makes, unless said.
    pub(crate) encryption_key_file: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Remote {
    pub(crate) address: Address,
    pub(crate) tokens: PathBuf,
    /// The certificate chain and key a `tls://` address answers with, PEM.
    pub(crate) tls: Option<(PathBuf, PathBuf)>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Transport {
    /// One client, the parent, on stdin and stdout.
    Stdio,
    /// A local socket or named pipe that `SERVE` names; a sidecar is one its
    /// clients started, which a client of a newer release may replace.
    Local { sidecar: bool },
}

pub(crate) fn serve(args: &[String]) -> Result<Serve, String> {
    let mut dir = None;
    let (mut stdio, mut local) = (false, false);
    let (mut log, mut idle, mut clock) = (None, None, None);
    let (mut listen, mut cert, mut key, mut tokens, mut memory) = (None, None, None, None, None);
    let (mut durability, mut keep_free, mut encryption_key_file) = (None, None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().cloned().ok_or_else(|| format!("{flag} takes a value"));
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(value("--dir")?)),
            "--stdio" => stdio = true,
            "--local" => local = true,
            "--log" => log = Some(PathBuf::from(value("--log")?)),
            "--idle" => idle = Some(span(&value("--idle")?)?),
            "--clock" => clock = Some(time(&value("--clock")?)?),
            "--listen" => listen = Some(Address::parse(&value("--listen")?)?),
            "--tls-cert" => cert = Some(PathBuf::from(value("--tls-cert")?)),
            "--tls-key" => key = Some(PathBuf::from(value("--tls-key")?)),
            "--tokens" => tokens = Some(PathBuf::from(value("--tokens")?)),
            "--memory" => memory = Some(size(&value("--memory")?)?),
            "--keep-free" => keep_free = Some(size(&value("--keep-free")?)?),
            "--encryption-key-file" => encryption_key_file = Some(PathBuf::from(value("--encryption-key-file")?)),
            "--durability" => {
                let word = value("--durability")?;
                durability = Some(word.parse::<Durability>().map_err(|_| format!("--durability {word}: full or os"))?);
            }
            flag if flag.starts_with("--") => return Err(format!("no flag {flag}")),
            _ if dir.is_none() => dir = Some(PathBuf::from(arg)),
            _ => return Err(format!("one directory, not {arg} too")),
        }
    }
    let dir = dir.ok_or("a store's directory: tinystore serve <dir>")?;
    let transport = match (stdio, local) {
        (true, true) => return Err("--stdio or --local, not both".to_owned()),
        (true, false) => Transport::Stdio,
        (false, sidecar) => Transport::Local { sidecar },
    };
    if clock.is_some() && transport != Transport::Stdio {
        return Err("--clock is a private server's: --stdio".to_owned());
    }
    let remote = match listen {
        None if cert.is_some() || key.is_some() || tokens.is_some() => {
            return Err("--tls-cert, --tls-key and --tokens are --listen's".to_owned());
        }
        None => None,
        Some(_) if transport == Transport::Stdio => return Err("--listen or --stdio, not both".to_owned()),
        Some(address) => Some(remote(address, tokens, cert, key)?),
    };
    let memory = memory.or(remote.as_ref().map(|_| REMOTE_MEMORY));
    let idle = match (idle, &transport) {
        (Some(Duration::ZERO), _) => None,
        (Some(idle), _) => Some(idle),
        (None, Transport::Local { sidecar: true }) => Some(SIDECAR_IDLE),
        (None, _) => None,
    };
    Ok(Serve { dir, transport, log, idle, clock, remote, memory, durability, keep_free, encryption_key_file })
}

fn remote(
    address: Address,
    tokens: Option<PathBuf>,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
) -> Result<Remote, String> {
    let tokens = tokens.ok_or("--listen needs --tokens: a remote client is admitted by its token")?;
    let tls = match (address.tls, cert, key) {
        (true, Some(cert), Some(key)) => Some((cert, key)),
        (true, ..) => return Err("a tls:// address needs --tls-cert and --tls-key".to_owned()),
        (false, None, None) => None,
        (false, ..) => return Err("--tls-cert and --tls-key are a tls:// address's".to_owned()),
    };
    Ok(Remote { address, tokens, tls })
}

/// A size as `--memory` takes it: bytes, or a number and KiB, MiB or GiB.
///
/// ```text
/// 1GiB → 1,073,741,824     512MiB → 536,870,912     4096 → 4,096
/// ```
fn size(text: &str) -> Result<u64, String> {
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let number: u64 = number.parse().map_err(|_| format!("the size {text}: as 1GiB or 512MiB"))?;
    let unit = match unit {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => return Err(format!("the size {text}: a unit of KiB, MiB or GiB")),
    };
    number.checked_mul(unit).ok_or_else(|| format!("the size {text}: past what a u64 holds"))
}

/// A span as `--idle` takes it: a number and its unit, `0` for ever.
///
/// ```text
/// 30000ms → 30 s     30s → 30 s     5m → 300 s     0 → for ever
/// ```
fn span(text: &str) -> Result<Duration, String> {
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let number: u64 = number.parse().map_err(|_| format!("the span {text}: a number and a unit, as 30s"))?;
    let millis = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "" if number == 0 => 0,
        _ => return Err(format!("the span {text}: a unit of ms, s, m or h")),
    };
    Ok(Duration::from_millis(number.saturating_mul(millis)))
}

/// A time as JavaScript's `toISOString` writes it, in UTC.
///
/// ```text
/// 2026-10-09T12:00:00.000Z → 1,791,547,200,000 ms     2026-10-09T12:00:00Z → the same
/// ```
fn time(text: &str) -> Result<SystemTime, String> {
    let refused = || format!("the time {text}: as 2026-10-09T12:00:00.000Z");
    let (date, rest) = text.split_once('T').ok_or_else(refused)?;
    let clock = rest.strip_suffix('Z').ok_or_else(refused)?;
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, "0"));
    let number = |part: &str| part.parse::<i64>().map_err(|_| refused());
    let day: Vec<i64> = date.split('-').map(number).collect::<Result<_, _>>()?;
    let hms: Vec<i64> = clock.split(':').map(number).collect::<Result<_, _>>()?;
    let (&[year, month, day], &[hour, minute, second]) = (&day[..], &hms[..]) else {
        return Err(refused());
    };
    let millis = format!("{fraction:0<3}")[..3].parse::<i64>().map_err(|_| refused())?;
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    let unix = u64::try_from(seconds * 1_000 + millis).map_err(|_| refused())?;
    Ok(UNIX_EPOCH + Duration::from_millis(unix))
}

/// The days from 1970-01-01 to a date of the proleptic Gregorian calendar,
/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_encryption_key_is_the_stores_own_file_unless_one_is_named() {
        assert_eq!(serve(&args("d")).unwrap().encryption_key_file, None);
        let named = serve(&args("d --encryption-key-file /run/secrets/tinystore.key")).unwrap();
        assert_eq!(named.encryption_key_file, Some(PathBuf::from("/run/secrets/tinystore.key")));
        assert_eq!(serve(&args("d --encryption-key-file")).unwrap_err(), "--encryption-key-file takes a value");
    }

    #[test]
    fn durability_is_full_or_os_and_each_engines_own_unless_said() {
        assert_eq!(serve(&args("d")).unwrap().durability, None);
        assert_eq!(serve(&args("d --durability os")).unwrap().durability, Some(Durability::Os));
        assert_eq!(serve(&args("--dir d --stdio --durability full")).unwrap().durability, Some(Durability::Full));
        assert_eq!(serve(&args("d --durability fast")).unwrap_err(), "--durability fast: full or os");
    }

    #[test]
    fn the_disk_a_store_keeps_free_is_the_stores_own_unless_said() {
        assert_eq!(serve(&args("d")).unwrap().keep_free, None);
        assert_eq!(serve(&args("d --keep-free 10GiB")).unwrap().keep_free, Some(10 << 30));
        assert_eq!(serve(&args("d --keep-free 0")).unwrap().keep_free, Some(0), "0 keeps none");
    }

    #[test]
    fn a_sidecar_stays_thirty_seconds_unless_told() {
        let sidecar = serve(&args("--dir d --local --log d/server/serve.log")).unwrap();
        assert_eq!(sidecar.transport, Transport::Local { sidecar: true });
        assert_eq!(sidecar.idle, Some(Duration::from_secs(30)));
        assert_eq!(serve(&args("--dir d --local --idle 1500ms")).unwrap().idle, Some(Duration::from_millis(1500)));
        assert_eq!(serve(&args("--dir d --local --idle 0ms")).unwrap().idle, None, "0 is for ever");
        let person = serve(&args("d")).unwrap();
        assert_eq!((person.transport, person.idle), (Transport::Local { sidecar: false }, None));
    }

    #[test]
    fn a_clock_is_a_private_servers() {
        let private = serve(&args("--dir d --stdio --clock 2026-10-09T12:00:00.000Z")).unwrap();
        assert_eq!(private.clock, Some(UNIX_EPOCH + Duration::from_millis(1_791_547_200_000)));
        assert!(serve(&args("--dir d --local --clock 2026-10-09T12:00:00.000Z")).is_err());
        assert!(serve(&args("--dir d --stdio --local")).is_err());
        assert!(serve(&args("--stdio")).is_err(), "no directory");
    }

    #[test]
    fn a_remote_server_needs_its_tokens_and_a_tls_address_its_certificate() {
        let tls = serve(&args("d --listen tls://0.0.0.0:7443 --tls-cert c.pem --tls-key k.pem --tokens t")).unwrap();
        assert_eq!(tls.remote.unwrap().tls, Some((PathBuf::from("c.pem"), PathBuf::from("k.pem"))));
        assert_eq!(tls.memory, Some(1 << 30), "a remote server's engines hold a GiB at most");
        assert!(serve(&args("d --listen tls://0.0.0.0:7443 --tokens t")).is_err(), "no certificate");
        assert!(serve(&args("d --listen tcp://127.0.0.1:0")).is_err(), "no tokens");
        assert!(serve(&args("d --listen tcp://127.0.0.1:0 --tokens t --tls-key k.pem")).is_err());
        assert!(serve(&args("d --tokens t")).is_err(), "tokens without --listen");
        assert!(serve(&args("--dir d --stdio --listen tcp://127.0.0.1:0 --tokens t")).is_err());
        let sized = serve(&args("d --listen tcp://127.0.0.1:0 --tokens t --memory 512MiB")).unwrap();
        assert_eq!(sized.memory, Some(512 << 20));
    }

    #[test]
    fn spans_and_times_read_as_the_sdks_write_them() {
        assert_eq!(span("30000ms").unwrap(), Duration::from_secs(30));
        assert_eq!(span("5m").unwrap(), Duration::from_secs(300));
        assert!(span("30").is_err(), "a unit");
        assert_eq!(time("1970-01-01T00:00:00Z").unwrap(), UNIX_EPOCH);
        assert_eq!(time("2000-03-01T00:00:00.5Z").unwrap(), UNIX_EPOCH + Duration::from_millis(951_868_800_500));
        assert!(time("2026-10-09 12:00:00").is_err());
    }
}
