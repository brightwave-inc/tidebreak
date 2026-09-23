fn main() {
    println!("cargo::rerun-if-env-changed=TIDEBREAK_VERSION");
    // `profile::Channel::current` reads it: a staging build keeps the staging
    // app's data directory and keychain service.
    println!("cargo::rerun-if-env-changed=TIDEBREAK_CHANNEL");
}
