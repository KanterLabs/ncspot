use crate::config::{user_cache_directory, user_configuration_directory};
use std::path::PathBuf;

/// Print platform info like which platform directories will be used.
pub fn info() -> Result<(), String> {
    let user_configuration_directory = user_configuration_directory();
    let user_cache_directory = user_cache_directory();

    println!(
        "USER_CONFIGURATION_PATH {}",
        user_configuration_directory
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or("not found".into())
    );
    println!(
        "USER_CACHE_PATH {}",
        user_cache_directory
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or("not found".into())
    );

    #[cfg(unix)]
    {
        use crate::utils::user_runtime_directory;

        let user_runtime_directory = user_runtime_directory();
        println!(
            "USER_RUNTIME_PATH {}",
            user_runtime_directory
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or("not found".into())
        );
    }

    Ok(())
}

/// Print a deterministic local radio recommendation report without logging in or making a
/// network request. A replay snapshot takes precedence over the seed controls.
pub fn radio_debug(
    seed: Option<String>,
    rng_seed: u64,
    limit: usize,
    replay: Option<PathBuf>,
) -> Result<(), String> {
    let report = if let Some(path) = replay {
        crate::recommendations::replay_report(&path)?
    } else {
        crate::recommendations::offline_report(seed, rng_seed, limit)?
    };
    log::debug!("radio-debug: {report}");
    println!("{report}");
    Ok(())
}
