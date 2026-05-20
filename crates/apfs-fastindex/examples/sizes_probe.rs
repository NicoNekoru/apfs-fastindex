use apfs_fastindex::tree::{Tree, TreeNode};
use apfs_fastindex::{EntryKind, NamespaceEntry};

fn main() {
    println!(
        "size_of NamespaceEntry = {}",
        std::mem::size_of::<NamespaceEntry>()
    );
    println!(
        "align_of NamespaceEntry = {}",
        std::mem::align_of::<NamespaceEntry>()
    );
    println!("size_of TreeNode = {}", std::mem::size_of::<TreeNode>());
    println!("align_of TreeNode = {}", std::mem::align_of::<TreeNode>());
    println!("size_of EntryKind = {}", std::mem::size_of::<EntryKind>());
    println!("size_of String = {}", std::mem::size_of::<String>());
    println!("size_of Box<str> = {}", std::mem::size_of::<Box<str>>());
    println!(
        "size_of Option<String> = {}",
        std::mem::size_of::<Option<String>>()
    );
    println!(
        "size_of Option<Box<str>> = {}",
        std::mem::size_of::<Option<Box<str>>>()
    );
    println!("size_of Vec<u32> = {}", std::mem::size_of::<Vec<u32>>());
    println!(
        "size_of Option<u64> = {}",
        std::mem::size_of::<Option<u64>>()
    );
}
