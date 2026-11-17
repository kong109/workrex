use super::*;
use crate::bin_name::BIN;

#[test]
fn own_pins_are_collected_unless_the_status_wrapper_or_shim_runs_them_and_upstreams_never() {
    let tmp = tempfile::tempdir().unwrap();
    let root = NodeRoot {
        path: tmp.path().to_path_buf(),
    };
    let bin = root.path.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (old, live) = (format!("{BIN}-0.1.277"), format!("{BIN}-0.1.278"));
    let (shimmed, dev) = (format!("{BIN}-0.1.276"), format!("{BIN}-dev-ba7816bf8f01"));
    let upstream = ["workrex-0.1.278", "workrex-dev-ba7816bf8f01"].map(String::from);
    for name in [&old, &live, &shimmed, &dev, &upstream[0], &upstream[1]] {
        fs::write(bin.join(name), "").unwrap();
    }
    super::super::execution::write_status_wrapper(&root, &bin.join(&live)).unwrap();
    let shim = tmp.path().join("shim");
    fs::write(&shim, super::super::path_shim::script(&bin.join(&shimmed))).unwrap();

    let pins = unused_pins(&root, &GcRequest::default(), &[], Some(&shim)).unwrap();
    assert_eq!(pins, vec![old.clone(), dev.clone()]);

    // Without the marker the file is the user's, and names nothing to keep.
    fs::write(
        &shim,
        format!("#!/bin/sh\nexec {} \"$@\"\n", bin.join(&shimmed).display()),
    )
    .unwrap();
    let pins = unused_pins(&root, &GcRequest::default(), &[], Some(&shim)).unwrap();
    assert_eq!(pins, vec![shimmed, old, dev]);
}

#[test]
fn a_pin_a_process_runs_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let root = NodeRoot {
        path: tmp.path().canonicalize().unwrap(),
    };
    let bin = root.path.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (running, idle) = (format!("{BIN}-0.1.277"), format!("{BIN}-0.1.276"));
    fs::copy(which::which("sleep").unwrap(), bin.join(&running)).unwrap();
    fs::write(bin.join(&idle), "").unwrap();
    let mut command = std::process::Command::new(bin.join(&running));
    // Linux reads the executable from /proc, so argv[0] can name `sleep` for
    // a multi-call coreutils (as in Nix), which picks its program by argv[0].
    // macOS `ps` reports argv[0], so it keeps the pin's path there.
    if cfg!(target_os = "linux") {
        std::os::unix::process::CommandExt::arg0(&mut command, "sleep");
    }
    let mut child = command.arg("30").spawn().unwrap();

    let pins = unused_pins(&root, &GcRequest::default(), &[], None);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(pins.unwrap(), vec![idle]);
}
