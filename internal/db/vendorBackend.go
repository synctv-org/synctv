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

func DeleteVendorBackend(endpoint string) error {
	return db.Where("backend_endpoint = ?", endpoint).Delete(&model.VendorBackend{}).Error
}

func DeleteVendorBackends(endpoints []string) error {
	return db.Where("backend_endpoint IN ?", endpoints).Delete(&model.VendorBackend{}).Error
}

func GetVendorBackend(endpoint string) (*model.VendorBackend, error) {
	var backend model.VendorBackend
	err := db.Where("backend_endpoint = ?", endpoint).First(&backend).Error
	return &backend, HandleNotFound(err, "backend")
}

func CreateOrSaveVendorBackend(backend *model.VendorBackend) (*model.VendorBackend, error) {
	return backend, Transactional(func(tx *gorm.DB) error {
		if err := tx.Where("backend_endpoint = ?", backend.Backend.Endpoint).First(&model.VendorBackend{}).Error; errors.Is(err, gorm.ErrRecordNotFound) {
			return tx.Create(&backend).Error
		} else {
			return tx.Save(&backend).Error
		}
	})
}

func SaveVendorBackend(backend *model.VendorBackend) error {
	return db.Save(backend).Error
}
