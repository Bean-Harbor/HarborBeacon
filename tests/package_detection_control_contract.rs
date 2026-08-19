use harborbeacon_local_agent::runtime::cat_detection_control::{
    CatDetectionControlPolicy, CatDetectionControlStore,
};
use harborbeacon_local_agent::runtime::package_detection_control::{
    PackageDetectionControlPolicy, PackageDetectionControlStore,
};

#[test]
fn cat_and_package_controls_persist_in_independent_files() {
    let root = std::env::temp_dir().join(format!(
        "harborbeacon-package-control-contract-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create test root");
    let cat_path = root.join("cat-controls.json");
    let package_path = root.join("package-controls.json");
    let cat_store = CatDetectionControlStore::try_new(cat_path).expect("cat store");
    let package_store = PackageDetectionControlStore::try_new(package_path).expect("package store");

    cat_store
        .upsert(CatDetectionControlPolicy::new("camera.252", false, "sub", 1).expect("cat policy"))
        .expect("persist cat policy");
    package_store
        .upsert(
            PackageDetectionControlPolicy::new("camera.252", true, "main", 2)
                .expect("package policy"),
        )
        .expect("persist package policy");

    let cats = cat_store.load().expect("load cat policies");
    let packages = package_store.load().expect("load package policies");
    assert!(!cats["camera.252"].desired_enabled);
    assert_eq!(cats["camera.252"].stream_profile, "sub");
    assert!(packages["camera.252"].desired_enabled);
    assert_eq!(packages["camera.252"].stream_profile, "main");
}
