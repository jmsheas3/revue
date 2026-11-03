// When the kernel's inotify queue overflows, events are lost, including the ones for directories created meanwhile.
// The watcher must rescan the tree then, or those directories stay unwatched until the server restarts.
#![cfg(target_os = "linux")]
mod common;
use common::*;

#[test]
fn an_overflow_rescans_the_tree_and_watches_directories_it_missed() {
    let dir = TempDir::new("overflow");
    let root = dir.path();
    init(root);
    write(root, "a.txt", "one\n");
    commit_all(root, "init");
    let s = Server::start(root);
    std::fs::create_dir_all(root.join("later/deeper")).unwrap();
    assert!(wait_until(3000, || s.watched().iter().any(|w| w == "later/deeper")), "{:?}", s.watched());

    // As if the events for later/ had been lost: its watches are gone and nothing told the watcher.
    s.debug(&[("forget", "later")]);
    let watched = s.watched();
    assert!(!watched.iter().any(|w| w.starts_with("later")), "{watched:?}");

    let ev = s.events();
    s.debug(&[("overflow", "1")]);
    let watched = s.watched();
    assert!(
        watched.iter().any(|w| w == "later") && watched.iter().any(|w| w == "later/deeper"),
        "not watched again after the overflow: {watched:?}"
    );
    // The overflow itself is reported as a change, since anything may have happened.
    assert_eq!(ev.next(2000).as_deref(), Some("git"));

    write(root, "later/deeper/x.txt", "x\n");
    assert!(ev.next(2000).is_some(), "no change event for a file in a rescanned directory");
}
