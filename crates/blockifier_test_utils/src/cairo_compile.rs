use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

use apollo_infra_utils::cairo_compiler_version::cairo1_compiler_version;
use apollo_infra_utils::compile_time_cargo_manifest_dir;
use tempfile::NamedTempFile;

use crate::contracts::TagAndToolchain;

const CAIRO0_PIP_REQUIREMENTS_FILE: &str = "tests/requirements.txt";
const CAIRO1_REPO_RELATIVE_PATH_OVERRIDE_ENV_VAR: &str = "CAIRO1_REPO_RELATIVE_PATH";
const DEFAULT_CAIRO1_REPO_RELATIVE_PATH: &str = "../../../cairo";

pub enum CompilationArtifacts {
    Cairo0 { casm: Vec<u8> },
    Cairo1 { casm: Vec<u8>, sierra: Vec<u8> },
}

pub fn cairo1_compiler_tag() -> String {
    // TODO(lior): Uncomment the following line it and remove the rest of the code, once the
    //   Cairo compiler version is updated to 2.11.0 in the toml file.
    //   If the compiler version is updated in the toml to a version < 2.11.0,
    //   only update the version in the assert below.
    // format!("v{}", cairo1_compiler_version())
    assert_eq!(cairo1_compiler_version(), "=2.10.0", "Unsupported compiler version.");
    "v2.11.0-dev.2".into()
}

/// Returns the path to the local Cairo1 compiler repository.
/// Returns <sequencer_repo_root>/<RELATIVE_PATH_TO_CAIRO_REPO>, where the relative path can be
/// overridden by the environment variable (otherwise, the default is used).
fn local_cairo1_compiler_repo_path() -> PathBuf {
    // Location of blockifier's Cargo.toml.
    let manifest_dir = compile_time_cargo_manifest_dir!();

    Path::new(&manifest_dir).join(
        env::var(CAIRO1_REPO_RELATIVE_PATH_OVERRIDE_ENV_VAR)
            .unwrap_or_else(|_| DEFAULT_CAIRO1_REPO_RELATIVE_PATH.into()),
    )
}

/// Runs a command. If it has succeeded, it returns the command's output; otherwise, it panics with
/// stderr output.
fn run_and_verify_output(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    if !output.status.success() {
        let stderr_output = String::from_utf8(output.stderr).unwrap();
        panic!("{stderr_output}");
    }
    output
}

/// Compiles a Cairo0 program using the deprecated compiler.
pub fn cairo0_compile(
    path: String,
    extra_arg: Option<String>,
    debug_info: bool,
) -> CompilationArtifacts {
    verify_cairo0_compiler_deps();
    let mut command = Command::new("starknet-compile-deprecated");
    command.arg(&path);
    if let Some(extra_arg) = extra_arg {
        command.arg(extra_arg);
    }
    if !debug_info {
        command.arg("--no_debug_info");
    }
    let compile_output = command.output().unwrap();
    let stderr_output = String::from_utf8(compile_output.stderr).unwrap();
    assert!(compile_output.status.success(), "{stderr_output}");
    CompilationArtifacts::Cairo0 { casm: compile_output.stdout }
}

/// Compiles a Cairo1 program using the compiler version set in the Cargo.toml.
pub fn cairo1_compile(
    path: String,
    git_tag_override: Option<String>,
    cargo_nightly_arg: Option<String>,
) -> CompilationArtifacts {
    let mut base_compile_args = vec![];

    let sierra_output =
        starknet_compile(path, git_tag_override, cargo_nightly_arg, &mut base_compile_args);

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(&sierra_output).unwrap();
    let temp_path_str = temp_file.into_temp_path();

    // Sierra -> CASM.
    let mut sierra_compile_command = Command::new("cargo");
    sierra_compile_command.args(base_compile_args);
    sierra_compile_command.args([
        "starknet-sierra-compile",
        temp_path_str.to_str().unwrap(),
        "--allowed-libfuncs-list-name",
        "all",
    ]);
    let casm_output = run_and_verify_output(&mut sierra_compile_command);

    CompilationArtifacts::Cairo1 { casm: casm_output.stdout, sierra: sierra_output }
}

/// Compile Cairo1 Contract into their Sierra version using the compiler version set in the
/// Cargo.toml
pub fn starknet_compile(
    path: String,
    git_tag_override: Option<String>,
    cargo_nightly_arg: Option<String>,
    base_compile_args: &mut Vec<String>,
) -> Vec<u8> {
    verify_cairo1_compiler_deps(git_tag_override);

    let cairo1_compiler_path = local_cairo1_compiler_repo_path();

    // Command args common to both compilation phases.
    base_compile_args.extend(vec![
        "run".into(),
        format!("--manifest-path={}/Cargo.toml", cairo1_compiler_path.to_string_lossy()),
        "--bin".into(),
    ]);
    // Add additional cargo arg if provided. Should be first arg (base command is `cargo`).
    if let Some(nightly_version) = cargo_nightly_arg {
        base_compile_args.insert(0, format!("+nightly-{nightly_version}"));
    }

    // Cairo -> Sierra.
    let mut starknet_compile_commmand = Command::new("cargo");
    starknet_compile_commmand.args(base_compile_args.clone());
    starknet_compile_commmand.args([
        "starknet-compile",
        "--",
        "--single-file",
        &path,
        "--allowed-libfuncs-list-name",
        "all",
    ]);
    let sierra_output = run_and_verify_output(&mut starknet_compile_commmand);

    sierra_output.stdout
}

/// Verifies that the required dependencies are available before compiling; panics if unavailable.
fn verify_cairo0_compiler_deps() {
    // Python compiler. Verify correct version.
    let cairo_lang_version_output =
        Command::new("sh").arg("-c").arg("pip freeze | grep cairo-lang").output().unwrap().stdout;
    let cairo_lang_version_untrimmed = String::from_utf8(cairo_lang_version_output).unwrap();
    let cairo_lang_version = cairo_lang_version_untrimmed.trim();
    let requirements_contents = fs::read_to_string(CAIRO0_PIP_REQUIREMENTS_FILE).unwrap();
    let expected_cairo_lang_version = requirements_contents
        .lines()
        .nth(1) // Skip docstring.
        .expect(
            "Expecting requirements file to contain a docstring in the first line, and \
            then the required cairo-lang version in the second line."
        ).trim();

    assert_eq!(
        cairo_lang_version,
        expected_cairo_lang_version,
        "cairo-lang version {expected_cairo_lang_version} not found ({}). Please run:\npip3.9 \
         install -r {}/{}\nthen rerun the test.",
        if cairo_lang_version.is_empty() {
            String::from("no installed cairo-lang found")
        } else {
            format!("installed version: {cairo_lang_version}")
        },
        compile_time_cargo_manifest_dir!(),
        CAIRO0_PIP_REQUIREMENTS_FILE
    );
}

fn get_tag_and_repo_file_path(git_tag_override: Option<String>) -> (String, PathBuf) {
    let tag = git_tag_override.unwrap_or(cairo1_compiler_tag());
    let cairo_repo_path = local_cairo1_compiler_repo_path();
    // Check if the path is a directory.
    assert!(
        cairo_repo_path.is_dir(),
        "Cannot verify Cairo1 contracts, Cairo repo not found at {0}.\nPlease run:\n\
        git clone https://github.com/starkware-libs/cairo {0}\nThen rerun the test.",
        cairo_repo_path.to_string_lossy(),
    );

    (tag, cairo_repo_path)
}

pub fn prepare_group_tag_compiler_deps(tag_and_toolchain: &TagAndToolchain) {
    let (optional_tag, optional_toolchain) = tag_and_toolchain;

    // Checkout the required version in the compiler repo.
    let (tag, cairo_repo_path) = get_tag_and_repo_file_path(optional_tag.clone());
    run_and_verify_output(Command::new("git").args([
        "-C",
        cairo_repo_path.to_str().unwrap(),
        "checkout",
        &tag,
    ]));

    // Install the toolchain, if specified.
    if let Some(toolchain) = optional_toolchain {
        run_and_verify_output(
            Command::new("rustup").args(["install", &format!("nightly-{toolchain}")]),
        );
    }
}

fn verify_cairo1_compiler_deps(git_tag_override: Option<String>) {
    let (tag, cairo_repo_path) = get_tag_and_repo_file_path(git_tag_override);

    // Verify that the checked out tag is as expected.
    run_and_verify_output(Command::new("git").args([
        "-C",
        cairo_repo_path.to_str().unwrap(),
        "rev-parse",
        "--verify",
        &tag,
    ]));
}
