use super::*;
use std::collections::HashMap;

const P: usize = 0x1000;
const A: usize = 0x2000;
const L: usize = 0x3000;
const N: usize = 0x4000;
const ROOT: usize = 0x5000;
fn fixture() -> HashMap<(usize, usize), u32> {
    HashMap::from([
        ((P, 0x20), 11), ((P, 0x144), 7), ((P, 0x30c), ROOT as u32),
        ((P, 0x420), A as u32), ((P, 0x424), 1), ((P, 0x428), 8),
        ((A, 0), L as u32),
        ((L, 0), LIGHT_VTABLE), ((L, 0x20), 23), ((L, 0x144), 7),
        ((L, 0x168), P as u32), ((L, 0x30c), N as u32),
        ((N, 0), 0x1234), ((N, 0x20), L as u32), ((N, 0x34), 7),
    ])
}
fn rd(map: &HashMap<(usize, usize), u32>, p: usize, o: usize) -> Option<u32> {
    map.get(&(p,o)).copied()
}
#[test]
fn exact_light_queue_then_hide_only_when_visual_rule_says_hide() {
    let map = fixture(); let cache = Mutex::new(Cache::new()); let any=AtomicBool::new(false); let mut queued = Vec::new();
    assert_eq!(refresh_core(&cache,&any, P, &|p,o| rd(&map,p,o), &|p| p==P, |p| queued.push(p)), 1);
    assert_eq!(queued, vec![L]);
    assert!(any.load(Ordering::Acquire));
    assert!(hidden_core(&cache, N, &|p,o| rd(&map,p,o), &|p| p==P, &|w| w==7));
    // Node+34 low three bits are native Light enable flags, not Geometry exception.
    assert_eq!(rd(&map,N,0x34), Some(7));
    assert!(!hidden_core(&cache, N, &|p,o| rd(&map,p,o), &|p| p==P, &|_| false));
    // A rule transition to Spy+goggles still queues original A37A40 to restore native state.
    assert_eq!(refresh_core(&cache,&any, P, &|p,o| rd(&map,p,o), &|p| p==P, |p| queued.push(p)), 1);
    assert_eq!(queued, vec![L,L]);
}
#[test]
fn ordinary_or_unregistered_parent_never_queues_or_hides() {
    let map = fixture(); let cache = Mutex::new(Cache::new()); let any=AtomicBool::new(false); let mut queued = Vec::new();
    assert_eq!(refresh_core(&cache,&any, P, &|p,o| rd(&map,p,o), &|_| false, |p| queued.push(p)), 0);
    assert!(queued.is_empty());
    assert!(!any.load(Ordering::Acquire));
    assert!(!hidden_core(&cache, N, &|p,o| rd(&map,p,o), &|_| false, &|_| true));
}
#[test]
fn reused_child_or_node_loses_permission() {
    let mut map = fixture(); let cache = Mutex::new(Cache::new()); let any=AtomicBool::new(false);
    assert_eq!(refresh_core(&cache,&any, P, &|p,o| rd(&map,p,o), &|_| true, |_| {}), 1);
    map.insert((L,0x20),24); // Same pointer, new light serial.
    assert!(!hidden_core(&cache,N,&|p,o| rd(&map,p,o),&|_|true,&|_|true));
    map.insert((L,0x20),23); map.insert((N,0x20),0x9999); // Node rebound.
    assert!(!hidden_core(&cache,N,&|p,o| rd(&map,p,o),&|_|true,&|_|true));
    map.insert((N,0x20),L as u32); map.insert((A,0),0x9999); // No longer in parent array.
    assert!(!hidden_core(&cache,N,&|p,o| rd(&map,p,o),&|_|true,&|_|true));
}
#[test]
fn invalid_array_or_parent_link_fails_native() {
    let mut map = fixture(); let cache = Mutex::new(Cache::new()); let any=AtomicBool::new(false); let mut calls=0;
    map.insert((P,0x424),9); // count > capacity 8.
    assert_eq!(refresh_core(&cache,&any,P,&|p,o|rd(&map,p,o),&|_|true,|_|calls+=1),0);
    map.insert((P,0x424),33); map.insert((P,0x428),40); // Beyond our per-parent bound.
    assert_eq!(refresh_core(&cache,&any,P,&|p,o|rd(&map,p,o),&|_|true,|_|calls+=1),0);
    map.insert((P,0x424),1); map.insert((L,0x168),0x9999);
    assert_eq!(refresh_core(&cache,&any,P,&|p,o|rd(&map,p,o),&|_|true,|_|calls+=1),0);
    assert_eq!(calls,0);
}
#[test]
fn parent_root_is_not_light_node() {
    let mut map = fixture(); let cache=Mutex::new(Cache::new()); let any=AtomicBool::new(false);
    map.insert((P,0x30c),N as u32);
    assert_eq!(refresh_core(&cache,&any,P,&|p,o|rd(&map,p,o),&|_|true,|_|{}),0);
}
#[test]
fn foreign_world_or_unknown_child_does_not_get_association() {
    let mut map=fixture(); let cache=Mutex::new(Cache::new()); let any=AtomicBool::new(false); let mut calls=0;
    map.insert((L,0x144),8);
    assert_eq!(refresh_core(&cache,&any,P,&|p,o|rd(&map,p,o),&|_|true,|_|calls+=1),0);
    map.insert((L,0x144),7); map.insert((L,0),0xdeadbeef);
    assert_eq!(refresh_core(&cache,&any,P,&|p,o|rd(&map,p,o),&|_|true,|_|calls+=1),0);
    assert_eq!(calls,0);
}
#[test]
fn ambiguous_node_removes_old_claim_and_rejects_new() {
    let mut cache=Cache::new();
    let first=LightId{parent:P,parent_key:11,world:7,light:L,light_key:23,node:N};
    let second=LightId{parent:P+1,parent_key:12,world:7,light:L+1,light_key:24,node:N};
    assert!(cache.insert(first));
    assert!(!cache.insert(second));
    assert_eq!(cache.by_node(N),None);
}

