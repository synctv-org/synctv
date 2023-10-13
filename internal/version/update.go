package version

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/cavaliergopher/grab/v3"
	log "github.com/sirupsen/logrus"
)

func SelfUpdate(ctx context.Context, url string) error {
	now := time.Now().UnixNano()
	currentExecFile, err := ExecutableFile()
	if err != nil {
		log.Errorf("self update: get current executable file error: %v", err)
		return err
	}
	log.Infof("self update: current executable file: %s", currentExecFile)

	tmp := filepath.Join(os.TempDir(), "synctv-server", fmt.Sprintf("self-update-%d", now))
	if err := os.MkdirAll(tmp, 0755); err != nil {
		log.Errorf("self update: mkdir %s error: %v", tmp, err)
		return err
	}
	log.Infof("self update: temp path: %s", tmp)
	defer func() {
		log.Infof("self update: remove temp path: %s", tmp)
		if err := os.RemoveAll(tmp); err != nil {
			log.Warnf("self update: remove temp path error: %v", err)
		}
	}()
	file, err := DownloadWithProgress(ctx, url, tmp)
	if err != nil {
		log.Errorf("self update: download %s error: %v", url, err)
		return err
	}
	log.Infof("self update: download success: %s", file)

	if err := os.Chmod(file, 0755); err != nil {
		log.Errorf("self update: chmod %s error: %v", file, err)
		return err
	}
	log.Infof("self update: chmod success: %s", file)

	oldName := fmt.Sprintf("%s-%d.old", currentExecFile, now)
	if err := os.Rename(currentExecFile, oldName); err != nil {
		log.Errorf("self update: rename %s -> %s error: %v", currentExecFile, oldName, err)
		return err
	}
	log.Infof("self update: rename success: %s -> %s", currentExecFile, oldName)

	defer func() {
		if err != nil {
			log.Infof("self update: rollback: %s -> %s", oldName, currentExecFile)
			if err := os.Rename(oldName, currentExecFile); err != nil {
				log.Errorf("self update: rollback: rename %s -> %s error: %v", oldName, currentExecFile, err)
			}
		} else {
			log.Infof("self update: remove old executable file: %s", oldName)
			if err := os.Remove(oldName); err != nil {
				log.Warnf("self update: remove old executable file error: %v", err)
			}
		}
	}()

	if err := os.Rename(file, currentExecFile); err != nil {
		log.Errorf("self update: rename %s -> %s error: %v", file, currentExecFile, err)
		return err
	}

	log.Infof("self update: update success: %s", currentExecFile)

	return nil
}

func DownloadWithProgress(ctx context.Context, url, path string) (string, error) {
	req, err := grab.NewRequest(path, url)
	if err != nil {
		return "", err
	}
	req = req.WithContext(ctx)
	resp := grab.NewClient().Do(req)
	t := time.NewTicker(250 * time.Millisecond)
	defer t.Stop()

	for {
		select {
		case <-t.C:
			log.Infof("self update: transferred %d / %d bytes (%.2f%%)",
				resp.BytesComplete(),
				resp.Size(),
				100*resp.Progress())

		case <-resp.Done:
			return resp.Filename, resp.Err()
		}
	}
}

// get current executable file
func ExecutableFile() (string, error) {
	p, err := os.Executable()
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(p)
}
