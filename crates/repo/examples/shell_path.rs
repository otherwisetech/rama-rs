//! Print the PATH rama gives git subprocesses (resolved from the login shell) and time it.
fn main() {
    let t = std::time::Instant::now();
    let path = repo::shell_env::path().unwrap_or_default().to_string();
    println!("{path}");
    eprintln!("{:.2?}; {} vars from the shell; {:?}", t.elapsed(), repo::shell_env::resolved().len(), repo::git_version());
}
