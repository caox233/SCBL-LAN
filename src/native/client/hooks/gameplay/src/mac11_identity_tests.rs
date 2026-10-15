use super::*;
struct Names {
    entry: Vec<u8>,
    table: [u32; 16],
    root: Box<u32>,
}
impl Names {
    fn new(name: &[u8]) -> Self {
        let mut entry = vec![0; 8];
        entry.extend_from_slice(name);
        entry.push(0);
        let mut value = Self { entry, table: [0; 16], root: Box::new(0) };
        value.table[7] = value.entry.as_ptr() as u32;
        value
    }
    unsafe fn matches(&mut self, id: u32, expected: &[u8]) -> bool {
        *self.root = self.table.as_ptr() as u32;
        native_name_is_at(id, expected, (&*self.root) as *const u32 as usize)
    }
}
#[test]
fn exact_asset_and_native_flag_match_but_numbered_names_do_not() {
    let mut names = Names::new(RETAIL_NAME);
    unsafe {
        for (id, expected) in [(7, true), (7 | (1 << 18), true), (7 | (1 << 19), false),
            (7 | (7 << 19), false), (7 | 0x80000000, false)] {
            assert_eq!(names.matches(id, RETAIL_NAME), expected);
        }
        assert!(!names.matches(7, b"ADV_Spy_SMG_MAC11_X"));
        assert!(!names.matches(8, RETAIL_NAME));
    }
}
#[test]
fn bone_names_use_same_exact_identity_without_asset_assumptions() {
    let mut names = Names::new(b"Gun_OneHanded");
    unsafe {
        assert!(names.matches(7, b"Gun_OneHanded"));
        assert!(names.matches(7 | (1 << 18), b"Gun_OneHanded"));
        assert!(!names.matches(7 | (1 << 19), b"Gun_OneHanded"));
        assert!(!names.matches(7, RETAIL_NAME));
    }
}
#[test]
fn invalid_root_entry_and_unterminated_text_fail_closed() {
    let mut names = Names::new(RETAIL_NAME);
    unsafe {
        assert!(!native_name_is_at(7, RETAIL_NAME, 0));
        assert!(!native_name_is_at(7 | (1 << 19), RETAIL_NAME, usize::MAX));
        names.table[7] = u32::MAX - 3;
        assert!(!names.matches(7, RETAIL_NAME));
        let bytes = [b'x'; 72];
        names.table[7] = bytes.as_ptr() as u32;
        assert!(!names.matches(7, RETAIL_NAME));
    }
}