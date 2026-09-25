package spike

import (
	"encoding/binary"
	"math"
	"slices"
)

const (
	recordNumberModels uint32 = 1 << iota
	recordStateModels
	recordJSONColumns
	recordBucketOrder
	recordScope16
	recordOuterCompression
	recordDeepAll = recordNumberModels | recordStateModels | recordJSONColumns | recordBucketOrder
)

func recordGCD(a, b uint64) uint64 {
	for b != 0 {
		a, b = b, a%b
	}
	return a
}

func (c *recordBlockCodec) modelRecordNumbers(values []uint64, best []byte) []byte {
	if len(values) < 3 {
		return best
	}
	ordinary := *c
	ordinary.features &^= recordNumberModels
	steps := make([]uint64, len(values)-1)
	minimum := slices.Min(values)
	divisor := uint64(0)
	for i := range steps {
		steps[i] = values[i+1] - values[i]
		divisor = recordGCD(divisor, values[i+1]-minimum)
	}
	divisor = recordGCD(divisor, values[0]-minimum)
	slices.Sort(steps)
	models := [][2]uint64{{values[0], 0}, {values[0], values[1] - values[0]}, {values[0], steps[len(steps)/2]}}
	if model, ok := fitRecordNumberLine(values); ok {
		models = append(models, model)
	}
	for _, model := range models {
		residuals := make([]uint64, len(values))
		for i, value := range values {
			residuals[i] = recordZigzag(value - (model[0] + uint64(i)*model[1]))
		}
		candidate := binary.AppendUvarint([]byte{4}, model[0])
		candidate = binary.AppendUvarint(candidate, model[1])
		candidate = append(candidate, ordinary.encodeNumbers(residuals)...)
		if len(c.pack(candidate)) < len(c.pack(best)) {
			best = candidate
		}
	}
	if divisor > 1 {
		quotients := make([]uint64, len(values))
		for i, value := range values {
			quotients[i] = (value - minimum) / divisor
		}
		candidate := binary.AppendUvarint([]byte{5}, minimum)
		candidate = binary.AppendUvarint(candidate, divisor)
		candidate = append(candidate, ordinary.encodeNumbers(quotients)...)
		if len(c.pack(candidate)) < len(c.pack(best)) {
			best = candidate
		}
	}
	for _, candidate := range [][]byte{ordinary.sparseRecordNumbers(values), ordinary.radixRecordNumbers(values)} {
		if candidate != nil && len(c.pack(candidate)) < len(c.pack(best)) {
			best = candidate
		}
	}
	return best
}

func fitRecordNumberLine(values []uint64) ([2]uint64, bool) {
	meanX := float64(len(values)-1) / 2
	meanY := 0.0
	for _, value := range values {
		meanY += float64(int64(value-values[0])) / float64(len(values))
	}
	covariance, variance := 0.0, 0.0
	for i, value := range values {
		x := float64(i) - meanX
		covariance += x * (float64(int64(value-values[0])) - meanY)
		variance += x * x
	}
	step := math.Round(covariance / variance)
	intercept := math.Round(meanY - step*meanX)
	if step < -0x1p63 || step >= 0x1p63 || intercept < -0x1p63 || intercept >= 0x1p63 {
		return [2]uint64{}, false
	}
	return [2]uint64{values[0] + uint64(int64(intercept)), uint64(int64(step))}, true
}

func readRecordNumberModel(cursor *recordCursor, mode, count int) []uint64 {
	base, parameter := cursor.unsigned(), cursor.unsigned()
	if len(cursor.data) == 0 || cursor.data[0] > 3 || (mode == 5 && parameter < 2) {
		cursor.fail("numeric model or nested model")
		return nil
	}
	values := readRecordNumbers(cursor, count)
	for i, value := range values {
		if mode == 4 {
			values[i] = base + uint64(i)*parameter + recordUnzigzag(value)
		} else {
			if value > ^uint64(0)/parameter {
				cursor.fail("numeric quotient overflow")
			}
			if value*parameter > ^uint64(0)-base {
				cursor.fail("numeric quotient base overflow")
			}
			values[i] = base + value*parameter
		}
	}
	return values
}
