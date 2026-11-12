use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");

    // `vendored` (default): build a pinned libsrt + mbedTLS from source.
    // Without it: link a system-installed libsrt discovered via pkg-config,
    // skipping the C build entirely.
    let include_dirs = if env::var_os("CARGO_FEATURE_VENDORED").is_some() {
        build_vendored()
    } else {
        probe_system()
    };

    generate_bindings(&include_dirs);
}

/// Build the vendored mbedTLS + libsrt statically, emit link directives, and
/// return the libsrt include directory for bindgen.
fn build_vendored() -> Vec<PathBuf> {
    // --- mbedTLS (static) ---------------------------------------------------
    let mbedtls = cmake::Config::new("vendor/mbedtls")
        .define("ENABLE_TESTING", "OFF")
        .define("ENABLE_PROGRAMS", "OFF")
        .define("USE_SHARED_MBEDTLS_LIBRARY", "OFF")
        .define("USE_STATIC_MBEDTLS_LIBRARY", "ON")
        // cmake 4.x rejects the old cmake_minimum_required the vendored trees declare.
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        .build();

    // --- libsrt (static) against that mbedTLS -------------------------------
    let srt = cmake::Config::new("vendor/libsrt")
        .define("ENABLE_APPS", "OFF")
        .define("ENABLE_SHARED", "OFF")
        .define("ENABLE_STATIC", "ON")
        .define("ENABLE_ENCRYPTION", "ON")
        .define("USE_ENCLIB", "mbedtls")
        .define("ENABLE_UNITTESTS", "OFF")
        .define("ENABLE_LOGGING", "ON")
        .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        .define("CMAKE_PREFIX_PATH", mbedtls.display().to_string())
        .define(
            "MBEDTLS_INCLUDE_DIR",
            format!("{}/include", mbedtls.display()),
        )
        .define("MBEDTLS_LIBRARY_DIR", format!("{}/lib", mbedtls.display()))
        .build();

    for dst in [&srt, &mbedtls] {
        println!("cargo:rustc-link-search=native={}/lib", dst.display());
        println!("cargo:rustc-link-search=native={}/lib64", dst.display());
    }
    println!("cargo:rustc-link-lib=static=srt");
    println!("cargo:rustc-link-lib=static=mbedtls");
    println!("cargo:rustc-link-lib=static=mbedx509");
    println!("cargo:rustc-link-lib=static=mbedcrypto");
    link_cpp_runtime();

    let include = PathBuf::from(format!("{}/include", srt.display()));
    println!("cargo:include={}", include.display());
    vec![include]
}

/// Locate a system libsrt via pkg-config (emitting its link directives) and
/// return its include directories for bindgen.
fn probe_system() -> Vec<PathBuf> {
    let lib = pkg_config::Config::new()
        .atleast_version("1.5.0")
        .probe("srt")
        .expect(
            "system libsrt not found via pkg-config. Install it (e.g. `brew install srt`, or \
             `apt-get install libsrt-openssl-dev`), ensure its `srt.pc` is on PKG_CONFIG_PATH, \
             or enable the default `vendored` feature to build libsrt from source.",
        );
    if let Some(dir) = lib.include_paths.first() {
        println!("cargo:include={}", dir.display());
    }
    lib.include_paths
}

/// libsrt is C++, so a statically linked build needs the C++ runtime. (Shared
/// system builds pull it in themselves; pkg-config handles that case.)
fn link_cpp_runtime() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "ios" | "tvos" | "watchos" | "freebsd" | "openbsd" => {
            println!("cargo:rustc-link-lib=c++");
        }
        _ => println!("cargo:rustc-link-lib=stdc++"),
    }
}

/// Generate the FFI bindings against the given include directories.
fn generate_bindings(include_dirs: &[PathBuf]) {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("srt_.*")
        .allowlist_type("SRT.*")
        .allowlist_type("srt_.*")
        .allowlist_var("SRT.*")
        .allowlist_var("srt_.*")
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        })
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for dir in include_dirs {
        builder = builder.clang_arg(format!("-I{}", dir.display()));
    }
    builder
        .generate()
        .expect("failed to generate libsrt bindings")
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}
