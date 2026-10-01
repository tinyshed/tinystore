package records

import "github.com/tinyshed/tinystore"

func (s *Store) Report() []tinystore.Measure {
	stats := s.Stats()
	return []tinystore.Measure{
		{Engine: "records", Name: "appended_records_total", Value: stats.Appended, Counter: true},
		{Engine: "records", Name: "dropped_records_total", Value: stats.Dropped, Counter: true},
		{Engine: "records", Name: "dropped_full_total", Value: stats.DroppedFull, Counter: true},
		{Engine: "records", Name: "dropped_invalid_total", Value: stats.DroppedInvalid, Counter: true},
		{Engine: "records", Name: "dropped_write_total", Value: stats.DroppedWrite, Counter: true},
		{Engine: "records", Name: "sealed_segments_total", Value: stats.SealedSegments, Counter: true},
		{Engine: "records", Name: "expired_segments_total", Value: stats.ExpiredSegments, Counter: true},
		{Engine: "records", Name: "merged_segments_total", Value: stats.MergedSegments, Counter: true},
		{Engine: "records", Name: "queries_total", Value: stats.Queries, Counter: true},
		{Engine: "records", Name: "read_blocks_total", Value: stats.ReadBlocks, Counter: true},
		{Engine: "records", Name: "read_bytes_total", Value: stats.ReadBytes, Counter: true},
		{Engine: "records", Name: "damaged_rows", Value: stats.Damaged},
	}
}
