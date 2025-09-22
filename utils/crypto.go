package utils

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"io"
)

func Crypto(v, key []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}

	// Use GCM as an AEAD mode instead of CFB
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}

	// Create a nonce for this encryption
	nonce := make([]byte, aead.NonceSize())
	if _, err := io.ReadFull(rand.Reader, nonce); err != nil {
		return nil, err
	}

	// Encrypt and authenticate the plaintext
	ciphertext := aead.Seal(nonce, nonce, v, nil)

	return ciphertext, nil
}

func Decrypto(v, key []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}

	// Use GCM as an AEAD mode instead of CFB
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}

	// Check if the ciphertext is at least as long as the nonce
	nonceSize := aead.NonceSize()
	if len(v) < nonceSize {
		return nil, errors.New("ciphertext too short")
	}

	// Extract the nonce from the ciphertext
	nonce, ciphertext := v[:nonceSize], v[nonceSize:]

	// Decrypt and verify the ciphertext
	plaintext, err := aead.Open(nil, nonce, ciphertext, nil)
	if err != nil {
		return nil, err
	}

	return plaintext, nil
}

func CryptoToBase64(v, key []byte) (string, error) {
	ciphertext, err := Crypto(v, key)
	if err != nil {
		return "", err
	}

	return base64.StdEncoding.EncodeToString(ciphertext), nil
}

func DecryptoFromBase64(v string, key []byte) ([]byte, error) {
	ciphertext, err := base64.StdEncoding.DecodeString(v)
	if err != nil {
		return nil, err
	}

	return Decrypto(ciphertext, key)
}

func GenCryptoKey(base string) []byte {
	key := make([]byte, 32)
	for i := range len(base) {
		key[i%32] ^= base[i]
	}

	return key
}

func GenCryptoKeyWithBytes(base []byte) []byte {
	key := make([]byte, 32)
	for i := range base {
		key[i%32] ^= base[i]
	}

	return key
}
