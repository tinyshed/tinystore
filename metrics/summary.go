package metrics

import (
	"math"
)

type blockSummary struct {
	last, min, max, sum, increase float64
	resets                        uint16
	valid                         bool
}

func summarize(points []Sample, kind Kind) blockSummary {
	summary := blockSummary{min: points[0].Value, max: points[0].Value, last: points[len(points)-1].Value, valid: true}
	for i, point := range points {
		value := point.Value
		if math.IsNaN(value) || math.IsInf(value, 0) {
			summary.valid = false
		}
		if kind == Gauge {
			summary.min = math.Min(summary.min, value)
			summary.max = math.Max(summary.max, value)
			summary.sum += value
		} else {
			if value < 0 {
				summary.valid = false
			}
			if i > 0 {
				previous := points[i-1].Value //nolint:gosec // guarded by i > 0
				if value < previous {
					summary.resets++
					summary.increase += value
				} else {
					summary.increase += value - previous
				}
			}
		}
	}
	if math.IsNaN(summary.sum) || math.IsInf(summary.sum, 0) ||
		math.IsNaN(summary.increase) || math.IsInf(summary.increase, 0) {
		summary.valid = false
	}
	return summary
}
