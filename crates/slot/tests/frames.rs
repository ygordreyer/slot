use slot::frames::Frames;

fn publish(frames: &Frames, value: u8, size: usize) {
    let mut buf = frames.take_write();
    buf.clear();
    buf.resize(size, value);
    frames.publish(buf, false);
}

#[test]
fn the_reader_gets_the_newest_frame_and_never_the_same_one_twice() {
    let f = Frames::new(4);
    for i in 0..50u8 {
        publish(&f, i, 4);
    }
    let latest = f.latest().expect("a published frame should be readable");
    assert_eq!(
        &latest[..],
        &[49u8; 4],
        "the reader was handed a stale frame"
    );
    drop(latest);
    assert!(
        f.latest().is_none(),
        "a consumed frame was handed out twice"
    );
}

#[test]
fn buffers_are_recycled_rather_than_reallocated_per_frame() {
    let f = Frames::new(4);
    publish(&f, 1, 4);
    let held = f.latest().expect("a published frame should be readable");
    for i in 0..500u32 {
        publish(&f, i as u8, 4);
    }
    drop(held);
    publish(&f, 0, 4);
    assert!(
        f.allocated() <= 3,
        "{} buffers in flight, the pool is leaking",
        f.allocated()
    );
}

#[test]
fn rewind_origin_stays_with_its_pixels_when_frames_are_replaced_and_recycled() {
    let frames = Frames::new(4);
    frames.publish(vec![1; 4], true);
    let held = frames.latest().unwrap();
    frames.publish(vec![2; 4], false);
    frames.publish(vec![3; 4], true);
    let latest = frames.latest().unwrap();
    assert_eq!(&latest[..], &[3; 4]);
    assert!(latest.rewound());
    assert_eq!(&held[..], &[1; 4]);
    assert!(held.rewound());
    drop(held);
    drop(latest);

    publish(&frames, 4, 4);
    let forward = frames.latest().unwrap();
    assert_eq!(&forward[..], &[4; 4]);
    assert!(!forward.rewound());
}
