use clap::builder::PathBufValueParser;
use librespot_playback::audio_backend;

pub const AUTHOR: &str = "KanterLabs Resonance contributors; based on ncspot by Henrik Friedrichsen <henrik@affekt.org> and contributors";
pub const BIN_NAME: &str = "resonance";
pub const DISPLAY_NAME: &str = "Resonance";
pub const CONFIGURATION_FILE_NAME: &str = "config.toml";
pub const USER_STATE_FILE_NAME: &str = "userstate.cbor";

/// Return the [Command](clap::Command) that models the program's command line arguments. The
/// command can be used to parse the actual arguments passed to the program, or to automatically
/// generate a man page using clap's mangen package.
pub fn program_arguments() -> clap::Command {
    let backends = {
        let backends: Vec<&str> = audio_backend::BACKENDS.iter().map(|b| b.0).collect();
        format!("Audio backends: {}", backends.join(", "))
    };

    let radio_debug = clap::Command::new("radio-debug")
        .about("Print a deterministic local radio recommendation report")
        .arg(
            clap::Arg::new("seed")
                .long("seed")
                .value_name("URI_OR_ID")
                .help("seed the report with a Spotify track URI or ID"),
        )
        .arg(
            clap::Arg::new("rng-seed")
                .long("rng-seed")
                .value_name("N")
                .value_parser(clap::value_parser!(u64))
                .default_value("42")
                .help("deterministic random seed (default: 42)"),
        )
        .arg(
            clap::Arg::new("limit")
                .long("limit")
                .value_name("N")
                .value_parser(clap::builder::RangedU64ValueParser::<usize>::new().range(1..=100))
                .default_value("20")
                .help("maximum number of candidates to report (1-100; default: 20)"),
        )
        .arg(
            clap::Arg::new("discovery")
                .long("discovery")
                .value_name("PERCENT")
                .value_parser(clap::builder::RangedU64ValueParser::<u8>::new().range(0..=100))
                .default_value("50")
                .conflicts_with("replay")
                .help("radio exploration level (0-100; default: 50)"),
        )
        .arg(
            clap::Arg::new("replay")
                .long("replay")
                .value_name("FILE")
                .value_parser(PathBufValueParser::new())
                .conflicts_with_all(["seed", "rng-seed", "limit", "discovery"])
                .help("replay an immutable radio-debug snapshot"),
        );

    clap::Command::new(BIN_NAME)
        .version(env!("VERSION"))
        .author(AUTHOR)
        .about("cross-platform ncurses Spotify client")
        .after_help(backends)
        .arg(
            clap::Arg::new("debug")
                .short('d')
                .long("debug")
                .value_name("FILE")
                .value_parser(PathBufValueParser::new())
                .help("Enable debug logging to the specified file"),
        )
        .arg(
            clap::Arg::new("basepath")
                .short('b')
                .long("basepath")
                .value_name("PATH")
                .value_parser(PathBufValueParser::new())
                .help("custom basepath to config/cache files"),
        )
        .arg(
            clap::Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Filename of config file in basepath")
                .default_value(CONFIGURATION_FILE_NAME),
        )
        .subcommands([
            clap::Command::new("info").about("Print platform information like paths"),
            radio_debug,
        ])
}

#[cfg(test)]
mod tests {
    use super::{BIN_NAME, program_arguments};

    #[test]
    fn parses_radio_debug_defaults_and_controls() {
        let matches = program_arguments()
            .try_get_matches_from([
                BIN_NAME,
                "radio-debug",
                "--seed",
                "spotify:track:abc",
                "--rng-seed",
                "42",
                "--limit",
                "20",
            ])
            .unwrap();
        let (_, subcommand) = matches.subcommand().unwrap();
        assert_eq!(
            subcommand.get_one::<String>("seed").unwrap(),
            "spotify:track:abc"
        );
        assert_eq!(*subcommand.get_one::<u64>("rng-seed").unwrap(), 42);
        assert_eq!(*subcommand.get_one::<usize>("limit").unwrap(), 20);
        assert_eq!(*subcommand.get_one::<u8>("discovery").unwrap(), 50);
        assert!(subcommand.get_one::<std::path::PathBuf>("replay").is_none());
    }

    #[test]
    fn radio_debug_limit_is_bounded_and_replay_is_exclusive() {
        assert!(
            program_arguments()
                .try_get_matches_from([BIN_NAME, "radio-debug", "--limit", "0"])
                .is_err()
        );
        assert!(
            program_arguments()
                .try_get_matches_from([BIN_NAME, "radio-debug", "--limit", "101"])
                .is_err()
        );
        assert!(
            program_arguments()
                .try_get_matches_from([
                    BIN_NAME,
                    "radio-debug",
                    "--replay",
                    "radio-replay.json",
                    "--seed",
                    "spotify:track:abc",
                ])
                .is_err()
        );
        assert!(
            program_arguments()
                .try_get_matches_from([BIN_NAME, "radio-debug", "--discovery", "101"])
                .is_err()
        );
        assert!(
            program_arguments()
                .try_get_matches_from([
                    BIN_NAME,
                    "radio-debug",
                    "--replay",
                    "radio-replay.json",
                    "--discovery",
                    "25",
                ])
                .is_err()
        );
    }
}
