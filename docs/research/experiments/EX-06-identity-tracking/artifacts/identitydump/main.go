package main

import (
	"crypto/sha256"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	apfs "github.com/blacktop/go-apfs"
	"github.com/blacktop/go-apfs/types"
)

type output struct {
	Device        string          `json:"device"`
	Volume        string          `json:"volume"`
	RootTree      objectIdentity  `json:"root_tree"`
	Nodes         []objectIdentity `json:"nodes"`
	NodeSummaries []nodeSummary    `json:"node_summaries"`
	Entries       []entry          `json:"entries"`
	RecordGroups  []recordGroup    `json:"record_groups"`
}

type objectIdentity struct {
	Domain      string `json:"domain"`
	Role        string `json:"role"`
	Oid         uint64 `json:"oid"`
	LookupXid   uint64 `json:"lookup_xid,omitempty"`
	ObjectXid   uint64 `json:"object_xid"`
	Paddr       uint64 `json:"paddr"`
	Checksum    uint64 `json:"checksum"`
	Type        string `json:"type"`
	Subtype     string `json:"subtype"`
	Flags       string `json:"flags"`
	Level       uint16 `json:"level,omitempty"`
	KeyCount    uint32 `json:"key_count,omitempty"`
	IsRoot      bool   `json:"is_root,omitempty"`
	IsLeaf      bool   `json:"is_leaf,omitempty"`
	ContentHash string `json:"content_hash"`
}

type entry struct {
	Path          string `json:"path"`
	Type          string `json:"type"`
	FileID        uint64 `json:"file_id"`
	LogicalSize   uint64 `json:"logical_size,omitempty"`
	SymlinkTarget string `json:"symlink_target,omitempty"`
}

type recordGroup struct {
	FileID      uint64   `json:"file_id"`
	RecordTypes []string `json:"record_types"`
	Names       []string `json:"names,omitempty"`
	LinkCount   int32    `json:"link_count,omitempty"`
	LogicalSize uint64   `json:"logical_size,omitempty"`
}

type nodeSummary struct {
	NodeKey          string         `json:"node_key"`
	Domain           string         `json:"domain"`
	Role             string         `json:"role"`
	Oid              uint64         `json:"oid"`
	LookupXid        uint64         `json:"lookup_xid,omitempty"`
	ObjectXid        uint64         `json:"object_xid"`
	Paddr            uint64         `json:"paddr"`
	Checksum         uint64         `json:"checksum"`
	Type             string         `json:"type"`
	Subtype          string         `json:"subtype"`
	Level            uint16         `json:"level,omitempty"`
	KeyCount         uint32         `json:"key_count,omitempty"`
	IsLeaf           bool           `json:"is_leaf"`
	RecordCounts     map[string]int `json:"record_counts"`
	MinFileID        uint64         `json:"min_file_id,omitempty"`
	MaxFileID        uint64         `json:"max_file_id,omitempty"`
	ChildOids        []uint64       `json:"child_oids,omitempty"`
	NameCount        int            `json:"name_count,omitempty"`
	NameSample       []string       `json:"name_sample,omitempty"`
	LogicalSizeTotal uint64         `json:"logical_size_total,omitempty"`
	SummaryHash      string         `json:"summary_hash"`
}

func main() {
	device := flag.String("device", "", "raw APFS container device path")
	flag.Parse()
	if *device == "" {
		fmt.Fprintln(os.Stderr, "missing --device")
		os.Exit(2)
	}

	result, err := dump(*device)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(result); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func dump(device string) (*output, error) {
	fs, err := apfs.Open(device)
	if err != nil {
		return nil, fmt.Errorf("open apfs: %w", err)
	}
	defer fs.Close()

	handle, err := os.Open(device)
	if err != nil {
		return nil, fmt.Errorf("open device: %w", err)
	}
	defer handle.Close()

	reader := io.NewSectionReader(handle, 0, 1<<63-1)
	fsOMAP := fs.Volume.OMap.Body.(types.OMap).Tree.Body.(types.BTreeNodePhys)

	rootEntry, err := fsOMAP.GetOMapEntry(reader, fs.Volume.RootTreeOid, types.XidT(^uint64(0)))
	if err != nil {
		return nil, fmt.Errorf("resolve root tree: %w", err)
	}
	rootObj, err := types.ReadObj(reader, rootEntry.Val.Paddr)
	if err != nil {
		return nil, fmt.Errorf("read root tree object: %w", err)
	}
	rootNode := rootObj.Body.(types.BTreeNodePhys)

	nodes, summaries, err := collectNodeIdentities(reader, fsOMAP, rootObj, rootNode, rootEntry.Val.Paddr, "fs_root_tree", 0)
	if err != nil {
		return nil, err
	}

	entries, err := walkDir(reader, fsOMAP, fs.FSRootBtree, types.OidT(types.FSROOT_OID), ".")
	if err != nil {
		return nil, err
	}
	slices.SortFunc(entries, func(a, b entry) int {
		return strings.Compare(a.Path, b.Path)
	})

	groups, err := recordGroups(reader, fsOMAP, fs.FSRootBtree, entries)
	if err != nil {
		return nil, err
	}

	return &output{
		Device:        device,
		Volume:        strings.TrimRight(string(fs.Volume.VolumeName[:]), "\x00"),
		RootTree:      nodes[0],
		Nodes:         nodes,
		NodeSummaries: summaries,
		Entries:       entries,
		RecordGroups:  groups,
	}, nil
}

func collectNodeIdentities(
	reader io.ReaderAt,
	fsOMAP types.BTreeNodePhys,
	obj *types.Obj,
	node types.BTreeNodePhys,
	paddr uint64,
	role string,
	depth int,
) ([]objectIdentity, []nodeSummary, error) {
	contentHash, err := blockHash(reader, paddr)
	if err != nil {
		return nil, nil, err
	}
	identity := objectIdentity{
		Domain:      "volume_omap",
		Role:        role,
		Oid:         uint64(obj.Hdr.Oid),
		ObjectXid:   uint64(obj.Hdr.Xid),
		Paddr:       paddr,
		Checksum:    obj.Hdr.Checksum(),
		Type:        fmt.Sprint(obj.Hdr.GetType()),
		Subtype:     fmt.Sprint(obj.Hdr.GetSubType()),
		Flags:       fmt.Sprint(obj.Hdr.GetFlag()),
		Level:       node.Level,
		KeyCount:    node.Nkeys,
		IsRoot:      node.IsRoot(),
		IsLeaf:      node.IsLeaf(),
		ContentHash: contentHash,
	}
	summary := summarizeNode(identity, node)
	identities := []objectIdentity{identity}
	summaries := []nodeSummary{summary}

	if node.IsLeaf() {
		return identities, summaries, nil
	}

	for idx, raw := range node.Entries {
		rec, ok := raw.(types.NodeEntry)
		if !ok {
			continue
		}
		childOID, ok := childOID(rec)
		if !ok {
			continue
		}
		omapEntry, err := fsOMAP.GetOMapEntry(reader, types.OidT(childOID), types.XidT(^uint64(0)))
		if err != nil {
			return nil, nil, fmt.Errorf("resolve child node %#x: %w", childOID, err)
		}
		childObj, err := types.ReadObj(reader, omapEntry.Val.Paddr)
		if err != nil {
			return nil, nil, fmt.Errorf("read child node %#x: %w", childOID, err)
		}
		childNode, ok := childObj.Body.(types.BTreeNodePhys)
		if !ok {
			continue
		}
		role := fmt.Sprintf("fs_tree_child_depth_%d_index_%d", depth+1, idx)
		childIdentities, childSummaries, err := collectNodeIdentities(reader, fsOMAP, childObj, childNode, omapEntry.Val.Paddr, role, depth+1)
		if err != nil {
			return nil, nil, err
		}
		for i := range childIdentities {
			childIdentities[i].LookupXid = uint64(omapEntry.Key.Xid)
		}
		for i := range childSummaries {
			childSummaries[i].LookupXid = uint64(omapEntry.Key.Xid)
		}
		identities = append(identities, childIdentities...)
		summaries = append(summaries, childSummaries...)
	}

	return identities, summaries, nil
}

func summarizeNode(identity objectIdentity, node types.BTreeNodePhys) nodeSummary {
	recordCounts := map[string]int{}
	var names []string
	var childOids []uint64
	var canonical []string
	var minFileID uint64
	var maxFileID uint64
	var logicalSizeTotal uint64

	for _, raw := range node.Entries {
		rec, ok := raw.(types.NodeEntry)
		if !ok {
			continue
		}

		recordType := fmt.Sprint(rec.Hdr.GetType())
		recordCounts[recordType]++
		fileID := rec.Hdr.GetID()
		if minFileID == 0 || fileID < minFileID {
			minFileID = fileID
		}
		if fileID > maxFileID {
			maxFileID = fileID
		}

		name := ""
		switch key := rec.Key.(type) {
		case types.JDrecHashedKeyT:
			name = key.Name
			names = append(names, name)
		case types.JXattrKeyT:
			name = key.Name
			names = append(names, name)
		}

		var child uint64
		if childOID, ok := childOID(rec); ok {
			child = childOID
			childOids = append(childOids, childOID)
		}

		var logicalSize uint64
		if inode, ok := rec.Val.(types.JInodeVal); ok {
			if size, ok := inodeLogicalSize(inode); ok {
				logicalSize = size
				logicalSizeTotal += size
			}
		}

		canonical = append(
			canonical,
			fmt.Sprintf("%016x|%s|%s|%016x|%d", fileID, recordType, name, child, logicalSize),
		)
	}

	slices.Sort(names)
	slices.Sort(childOids)
	slices.Sort(canonical)
	sum := sha256.Sum256([]byte(strings.Join(canonical, "\n")))
	nameSample := names
	if len(nameSample) > 20 {
		nameSample = nameSample[:20]
	}

	return nodeSummary{
		NodeKey:          identityKey(identity),
		Domain:           identity.Domain,
		Role:             identity.Role,
		Oid:              identity.Oid,
		LookupXid:        identity.LookupXid,
		ObjectXid:        identity.ObjectXid,
		Paddr:            identity.Paddr,
		Checksum:         identity.Checksum,
		Type:             identity.Type,
		Subtype:          identity.Subtype,
		Level:            identity.Level,
		KeyCount:         identity.KeyCount,
		IsLeaf:           identity.IsLeaf,
		RecordCounts:     recordCounts,
		MinFileID:        minFileID,
		MaxFileID:        maxFileID,
		ChildOids:        childOids,
		NameCount:        len(names),
		NameSample:       nameSample,
		LogicalSizeTotal: logicalSizeTotal,
		SummaryHash:      fmt.Sprintf("%x", sum[:]),
	}
}

func identityKey(identity objectIdentity) string {
	return fmt.Sprintf(
		"%s|%d|%d|%d|%d|%s|%s|%s",
		identity.Domain,
		identity.Oid,
		identity.ObjectXid,
		identity.Paddr,
		identity.Checksum,
		identity.Type,
		identity.Subtype,
		identity.ContentHash,
	)
}

func blockHash(reader io.ReaderAt, paddr uint64) (string, error) {
	block := make([]byte, types.BLOCK_SIZE)
	if _, err := reader.ReadAt(block, int64(paddr*types.BLOCK_SIZE)); err != nil {
		return "", fmt.Errorf("read block %#x for hash: %w", paddr, err)
	}
	sum := sha256.Sum256(block)
	return fmt.Sprintf("%x", sum[:]), nil
}

func childOID(rec types.NodeEntry) (uint64, bool) {
	switch val := rec.Val.(type) {
	case uint64:
		return val, true
	case types.BTreeNodeIndexNodeValT:
		return uint64(val.ChildOid), true
	default:
		return 0, false
	}
}

func walkDir(
	reader io.ReaderAt,
	fsOMAP types.BTreeNodePhys,
	fsRoot types.BTreeNodePhys,
	dirOID types.OidT,
	dirPath string,
) ([]entry, error) {
	records, err := fsOMAP.GetFSRecordsForOid(reader, fsRoot, dirOID, types.XidT(^uint64(0)))
	if err != nil {
		return nil, fmt.Errorf("load directory oid %#x: %w", uint64(dirOID), err)
	}

	var entries []entry
	for _, record := range records {
		if record.Hdr.GetType() != types.APFS_TYPE_DIR_REC {
			continue
		}

		key := record.Key.(types.JDrecHashedKeyT)
		val := record.Val.(types.JDrecVal)
		if key.Name == ".fseventsd" {
			continue
		}

		childPath := key.Name
		if dirPath != "." {
			childPath = filepath.Join(dirPath, key.Name)
		}

		current := entry{
			Path:   childPath,
			Type:   recordType(val.Flags),
			FileID: uint64(val.FileID),
		}

		childRecords, err := fsOMAP.GetFSRecordsForOid(
			reader,
			fsRoot,
			types.OidT(val.FileID),
			types.XidT(^uint64(0)),
		)
		if err != nil {
			return nil, fmt.Errorf("load child oid %#x: %w", uint64(val.FileID), err)
		}

		populateMetadata(&current, childRecords)
		entries = append(entries, current)

		if current.Type == "dir" {
			childEntries, err := walkDir(reader, fsOMAP, fsRoot, types.OidT(val.FileID), childPath)
			if err != nil {
				return nil, err
			}
			entries = append(entries, childEntries...)
		}
	}

	return entries, nil
}

func populateMetadata(out *entry, records types.FSRecords) {
	for _, record := range records {
		switch record.Hdr.GetType() {
		case types.APFS_TYPE_INODE:
			inode := record.Val.(types.JInodeVal)
			if size, ok := inodeLogicalSize(inode); ok {
				out.LogicalSize = size
			}
		case types.APFS_TYPE_XATTR:
			if out.Type != "symlink" {
				continue
			}
			key := record.Key.(types.JXattrKeyT)
			if key.Name != types.XATTR_SYMLINK_EA_NAME {
				continue
			}
			if data, ok := record.Val.(types.JXattrValT).Data.([]byte); ok {
				out.SymlinkTarget = strings.TrimRight(string(data), "\x00")
			}
		}
	}

	if out.Type == "symlink" && out.LogicalSize == 0 && out.SymlinkTarget != "" {
		out.LogicalSize = uint64(len(out.SymlinkTarget))
	}
}

func inodeLogicalSize(inode types.JInodeVal) (uint64, bool) {
	for _, field := range inode.Xfields {
		if field.XType == types.INO_EXT_TYPE_DSTREAM {
			return field.Field.(types.JDstreamT).Size, true
		}
	}
	if inode.InternalFlags&types.INODE_HAS_UNCOMPRESSED_SIZE != 0 {
		return inode.UncompressedSize, true
	}
	return 0, false
}

func recordType(flag interface{ String() string }) string {
	switch flag.String() {
	case types.DT_DIR.String():
		return "dir"
	case types.DT_REG.String():
		return "file"
	case types.DT_LNK.String():
		return "symlink"
	case types.DT_FIFO.String():
		return "other(DT_FIFO)"
	default:
		return "other(" + flag.String() + ")"
	}
}

func recordGroups(
	reader io.ReaderAt,
	fsOMAP types.BTreeNodePhys,
	fsRoot types.BTreeNodePhys,
	entries []entry,
) ([]recordGroup, error) {
	seen := map[uint64]bool{}
	ids := []uint64{uint64(types.FSROOT_OID)}
	for _, entry := range entries {
		if !seen[entry.FileID] {
			ids = append(ids, entry.FileID)
			seen[entry.FileID] = true
		}
	}
	slices.Sort(ids)

	var groups []recordGroup
	for _, id := range ids {
		records, err := fsOMAP.GetFSRecordsForOid(reader, fsRoot, types.OidT(id), types.XidT(^uint64(0)))
		if err != nil {
			return nil, err
		}
		group := recordGroup{FileID: id}
		for _, record := range records {
			group.RecordTypes = append(group.RecordTypes, fmt.Sprint(record.Hdr.GetType()))
			switch record.Hdr.GetType() {
			case types.APFS_TYPE_INODE:
				inode := record.Val.(types.JInodeVal)
				group.LinkCount = inode.NchildrenOrNlink
				if size, ok := inodeLogicalSize(inode); ok {
					group.LogicalSize = size
				}
			case types.APFS_TYPE_DIR_REC:
				group.Names = append(group.Names, record.Key.(types.JDrecHashedKeyT).Name)
			case types.APFS_TYPE_XATTR:
				group.Names = append(group.Names, record.Key.(types.JXattrKeyT).Name)
			}
		}
		slices.Sort(group.RecordTypes)
		slices.Sort(group.Names)
		groups = append(groups, group)
	}
	return groups, nil
}
