package main

import (
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

type entry struct {
	Path          string `json:"path"`
	Type          string `json:"type"`
	FileID        uint64 `json:"file_id"`
	LogicalSize   uint64 `json:"logical_size,omitempty"`
	SymlinkTarget string `json:"symlink_target,omitempty"`
}

type output struct {
	Device     string  `json:"device"`
	Volume     string  `json:"volume"`
	EntryCount int     `json:"entry_count"`
	Entries    []entry `json:"entries"`
}

func main() {
	device := flag.String("device", "", "raw APFS container device path")
	flag.Parse()
	if *device == "" {
		fmt.Fprintln(os.Stderr, "missing --device")
		os.Exit(2)
	}

	result, err := walk(*device)
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

func walk(device string) (*output, error) {
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

	entries, err := walkDir(reader, fsOMAP, fs.FSRootBtree, types.OidT(types.FSROOT_OID), ".")
	if err != nil {
		return nil, err
	}

	slices.SortFunc(entries, func(a, b entry) int {
		return strings.Compare(a.Path, b.Path)
	})

	return &output{
		Device:     device,
		Volume:     strings.TrimRight(string(fs.Volume.VolumeName[:]), "\x00"),
		EntryCount: len(entries),
		Entries:    entries,
	}, nil
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
	default:
		return "other(" + flag.String() + ")"
	}
}
