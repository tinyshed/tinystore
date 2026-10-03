package wire

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
)

// the bytes of a start's instance and secret, and of a HELLO's challenge
const (
	InstanceSize  = 16
	SecretSize    = 32
	ChallengeSize = 16
)

// Published is SERVE, the file in <dir>/server/ where a store's sidecar says
// where it listens. Only the directory's owner can read it, so its secret is
// what a client that found the sidecar there checks it by before its first
// request: a process that took the endpoint of a server gone cannot prove it.
type Published struct {
	Protocol  int      `json:"protocol"`
	Server    string   `json:"server"`
	PID       int      `json:"pid"`
	Instance  string   `json:"instance"` // InstanceSize bytes, base64url without padding
	Secret    string   `json:"secret"`   // SecretSize bytes, base64url without padding
	Endpoints []string `json:"endpoints"`

	// Sidecar is a server started for its clients, which a client of a newer
	// release stops and starts its own in its place; a program's own server
	// and one a person runs say nothing.
	Sidecar bool `json:"sidecar,omitempty"`
}

// Prove is a WELCOME's answer to a HELLO's challenge: an HMAC-SHA256 of the
// challenge, keyed with the secret SERVE names.
func Prove(secret, challenge []byte) []byte {
	mac := hmac.New(sha256.New, secret)
	mac.Write(challenge)
	return mac.Sum(nil)
}

// Proves says whether a WELCOME's proof answers the challenge with the secret
// SERVE names, in constant time. A SERVE without a whole secret proves nothing.
func (p Published) Proves(challenge, proof []byte) bool {
	secret, err := base64.RawURLEncoding.DecodeString(p.Secret)
	if err != nil || len(secret) != SecretSize || len(challenge) != ChallengeSize {
		return false
	}
	return hmac.Equal(Prove(secret, challenge), proof)
}
