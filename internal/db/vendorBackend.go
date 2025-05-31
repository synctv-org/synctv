package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

func GetAllVendorBackend() ([]*model.VendorBackend, error) {
	var backends []*model.VendorBackend
	err := db.Find(&backends).Error
	return backends, HandleNotFound(err, "backends")
}

func CreateVendorBackend(backend *model.VendorBackend) error {
	return db.Create(backend).Error
}

func updateVendorBackendEnabled(endpoint string, enabled bool) error {
	result := db.Model(&model.VendorBackend{}).
		Where("backend_endpoint = ?", endpoint).
		Update("enabled", enabled)
	return HandleUpdateResult(result, "vendor backend")
}

func EnableVendorBackend(endpoint string) error {
	return updateVendorBackendEnabled(endpoint, true)
}

func EnableVendorBackends(endpoints []string) error {
	result := db.Model(&model.VendorBackend{}).
		Where("backend_endpoint IN ?", endpoints).
		Update("enabled", true)
	return HandleUpdateResult(result, "vendor backends")
}

func DisableVendorBackend(endpoint string) error {
	return updateVendorBackendEnabled(endpoint, false)
}

func DisableVendorBackends(endpoints []string) error {
	result := db.Model(&model.VendorBackend{}).
		Where("backend_endpoint IN ?", endpoints).
		Update("enabled", false)
	return HandleUpdateResult(result, "vendor backends")
}

func DeleteVendorBackend(endpoint string) error {
	result := db.Where("backend_endpoint = ?", endpoint).Delete(&model.VendorBackend{})
	return HandleUpdateResult(result, "vendor backend")
}

func DeleteVendorBackends(endpoints []string) error {
	result := db.Where("backend_endpoint IN ?", endpoints).Delete(&model.VendorBackend{})
	return HandleUpdateResult(result, "vendor backends")
}

func GetVendorBackend(endpoint string) (*model.VendorBackend, error) {
	var backend model.VendorBackend
	err := db.Where("backend_endpoint = ?", endpoint).First(&backend).Error
	return &backend, HandleNotFound(err, "backend")
}

func CreateOrSaveVendorBackend(backend *model.VendorBackend) (*model.VendorBackend, error) {
	return backend, Transactional(func(tx *gorm.DB) error {
		var existingBackend model.VendorBackend
		err := tx.Where("backend_endpoint = ?", backend.Backend.Endpoint).
			First(&existingBackend).
			Error
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return tx.Create(backend).Error
		} else if err != nil {
			return err
		}
		result := tx.Model(&existingBackend).Omit("created_at").Updates(backend)
		return HandleUpdateResult(result, "vendor backend")
	})
}

func SaveVendorBackend(backend *model.VendorBackend) error {
	result := db.Omit("created_at").Save(backend)
	return HandleUpdateResult(result, "vendor backend")
}
