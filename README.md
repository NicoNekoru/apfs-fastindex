# APFS-FastIndex

Attempting to create as fast of a WizTree alternative as possible for MacOS/APFS disk format.

## Motivations

The blazingly-fast speed of WizTree's drive indexing relies on the convenience of NTFS metadata, i.e. that NTFS keeps a Master File Table (MFT). The MFT is a single, flat structure in which each file on the drive is stored as a record in the table. As a result, we can sequentially scan this table directly, and don't need to traverse the drive or get stuck recursively searching subdirectories.

Apple's disk format, APFS, does not have this. Instead of indexing metadata with a flat table, the APFS superblock uses B-trees as object maps, thereby spreading metadata across three different structures: the object map (OMAP), FS tree, and extent tree. Everything in this record is copy-on-write and transactional, relying on sparse object IDs rather than a convenient linear record.

As a result, it is easiest to interface with the APFS drive via filesystem APIs. However, `readdir`, `fstatat`, and even `getattrlistbulk` are not exactly purpose built/ideal for a full drive index. Outside of these abstractions, documentation is extremely poor, and there are no lower level APIs.

This is a project to reverse engineer APFS structures directly in order to get the most optimal APFS indexing as possible.
