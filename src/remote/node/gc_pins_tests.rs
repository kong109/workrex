use super::*;
use crate::bin_name::BIN;

#[test]
fn release_pins_are_collected_unless_the_status_wrapper_runs_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = NodeRoot {
        path: tmp.path().to_path_buf(),
    };
    let bin = root.path.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (old, live) = (format!("{BIN}-0.1.277"), format!("{BIN}-0.1.278"));
    for name in [&old, &live, &"workmux-dev-ba7816bf8f01".to_string()] {
        fs::write(bin.join(name), "").unwrap();
    }
    super::super::execution::write_status_wrapper(&root, &bin.join(&live)).unwrap();

    let pins = unused_pins(&root, &GcRequest::default(), &[]).unwrap();

    assert_eq!(pins, vec!["workmux-dev-ba7816bf8f01".to_string(), old]);
}
