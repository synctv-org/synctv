package utils

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"io"
)

func Crypto(v []byte, key []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}

	ciphertext := make([]byte, aes.BlockSize+len(v))
	iv := ciphertext[:aes.BlockSize]
	if _, err := io.ReadFull(rand.Reader, iv); err != nil {
		return nil, err
	}

	stream := cipher.NewCFBEncrypter(block, iv)
	stream.XORKeyStream(ciphertext[aes.BlockSize:], v)

	return ciphertext, nil
}

func Decrypto(v []byte, key []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}

	if len(v) < aes.BlockSize {
		return nil, errors.New("ciphertext too short")
	}
	iv := v[:aes.BlockSize]
	v = v[aes.BlockSize:]

	stream := cipher.NewCFBDecrypter(block, iv)
	stream.XORKeyStream(v, v)

	return v, nil
}

func CryptoToBase64(v []byte, key []byte) (string, error) {
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
	for i := 0; i < len(base); i++ {
		key[i%32] ^= base[i]
	}
	return key
}
