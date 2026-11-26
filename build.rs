fn main() {
    println!("cargo:rerun-if-env-changed=POLYDIRECT_GITHUB_REPO");
    println!("cargo:rerun-if-env-changed=POLYDESK_GITHUB_REPO");

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=POLYDESK_TARGET={target}");

    let gh_repo = std::env::var("POLYDIRECT_GITHUB_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("POLYDESK_GITHUB_REPO")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "Mluo2011/polydirect".to_string());
    println!("cargo:rustc-env=POLYDESK_DEFAULT_GITHUB_REPO={gh_repo}");
}
