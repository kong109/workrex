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
    let upstream = ["workmux-0.1.278", "workmux-dev-ba7816bf8f01"].map(String::from);
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
