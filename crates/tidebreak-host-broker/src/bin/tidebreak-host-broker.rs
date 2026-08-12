//! Tidebreak host-broker sidecar process.

use std::{
    ffi::{OsStr, OsString},
    io::{self, BufReader, BufWriter},
    path::PathBuf,
    process::ExitCode,
};

use tidebreak_host_broker::{sidecar, Broker, RootPolicy};

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("tidebreak-host-broker: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = std::fs::create_dir_all(&args.data_dir) {
        eprintln!("tidebreak-host-broker: could not create private data directory: {error}");
        return ExitCode::FAILURE;
    }
    let data_dir = match args.data_dir.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("tidebreak-host-broker: could not resolve private data directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    let policy = match RootPolicy::for_host(args.home)
        .and_then(|policy| policy.with_private_directory(&data_dir))
    {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("tidebreak-host-broker: could not initialize root policy: {error}");
            return ExitCode::FAILURE;
        }
    };
    let broker = match Broker::open_with_execute_commands(policy, &data_dir, args.execute_commands)
    {
        Ok(broker) => broker,
        Err(error) => {
            eprintln!("tidebreak-host-broker: could not open broker state: {error}");
            return ExitCode::FAILURE;
        }
    };
    let input = BufReader::new(io::stdin().lock());
    let output = BufWriter::new(io::stdout().lock());
    match sidecar::serve(&broker, input, output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tidebreak-host-broker: sidecar I/O failed: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    data_dir: PathBuf,
    home: PathBuf,
    execute_commands: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, &'static str> {
        let mut data_dir = None;
        let mut home = None;
        let mut execute_commands = false;
        while let Some(argument) = args.next() {
            if argument == OsStr::new("--execute-commands") {
                if execute_commands {
                    return Err("duplicate sidecar argument");
                }
                execute_commands = true;
                continue;
            }
            let destination = match argument.as_os_str() {
                value if value == OsStr::new("--data-dir") && data_dir.is_none() => &mut data_dir,
                value if value == OsStr::new("--home") && home.is_none() => &mut home,
                value if value == OsStr::new("--data-dir") || value == OsStr::new("--home") => {
                    return Err("duplicate sidecar argument")
                }
                _ => return Err("unknown sidecar argument"),
            };
            *destination = Some(PathBuf::from(
                args.next().ok_or("sidecar argument requires a value")?,
            ));
        }
        let data_dir = data_dir.ok_or("missing --data-dir")?;
        let home = home.ok_or("missing --home")?;
        if !data_dir.is_absolute() || !home.is_absolute() {
            return Err("sidecar paths must be absolute");
        }
        Ok(Self {
            data_dir,
            home,
            execute_commands,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_require_one_absolute_value_per_path() {
        let base = std::env::temp_dir();
        let data_dir = base.join("tidebreak");
        let home = base.join("home");
        let parsed = Args::parse(
            [
                OsString::from("--data-dir"),
                data_dir.clone().into_os_string(),
                OsString::from("--home"),
                home.clone().into_os_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(parsed.data_dir, data_dir);
        assert!(Args::parse(
            [
                OsString::from("--data-dir"),
                OsString::from("relative"),
                OsString::from("--home"),
                home.clone().into_os_string(),
            ]
            .into_iter()
        )
        .is_err());
        assert!(Args::parse(
            [
                OsString::from("--home"),
                home.clone().into_os_string(),
                OsString::from("--home"),
                home.into_os_string(),
            ]
            .into_iter()
        )
        .is_err());
    }
}
