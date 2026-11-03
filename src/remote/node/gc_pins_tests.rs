use super::*;
use crate::bin_name::BIN;

#[test]
fn own_pins_are_collected_unless_the_status_wrapper_runs_them_and_upstreams_never() {
    let tmp = tempfile::tempdir().unwrap();
    let root = NodeRoot {
        path: tmp.path().to_path_buf(),
    };
    let bin = root.path.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (old, live) = (format!("{BIN}-0.1.277"), format!("{BIN}-0.1.278"));
    let dev = format!("{BIN}-dev-ba7816bf8f01");
    let upstream = ["workmux-0.1.278", "workmux-dev-ba7816bf8f01"].map(String::from);
    for name in [&old, &live, &dev, &upstream[0], &upstream[1]] {
        fs::write(bin.join(name), "").unwrap();
    }
    super::super::execution::write_status_wrapper(&root, &bin.join(&live)).unwrap();

    let pins = unused_pins(&root, &GcRequest::default(), &[]).unwrap();

    assert_eq!(pins, vec![old, dev]);
}
