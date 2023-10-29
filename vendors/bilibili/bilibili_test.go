package bilibili_test

import (
	"testing"

	"github.com/synctv-org/synctv/vendors/bilibili"
)

func TestMatch(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name    string
		url     string
		wantT   string
		wantID  string
		wantErr bool
	}{
		{
			name:    "bv",
			url:     "https://www.bilibili.com/video/BV1i5411y7fB",
			wantT:   "bv",
			wantID:  "BV1i5411y7fB",
			wantErr: false,
		},
		{
			name:    "av",
			url:     "https://www.bilibili.com/video/av1",
			wantT:   "av",
			wantID:  "1",
			wantErr: false,
		},
		{
			name:    "ss",
			url:     "https://www.bilibili.com/bangumi/play/ss1",
			wantT:   "ss",
			wantID:  "1",
			wantErr: false,
		},
		{
			name:    "ep",
			url:     "https://www.bilibili.com/bangumi/play/ep1",
			wantT:   "ep",
			wantID:  "1",
			wantErr: false,
		},
	}
	for _, tt := range tests {
		tt := tt
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			gotT, gotID, err := bilibili.Match(tt.url)
			if (err != nil) != tt.wantErr {
				t.Errorf("Match() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if gotT != tt.wantT {
				t.Errorf("Match() gotT = %v, want %v", gotT, tt.wantT)
			}
			if gotID != tt.wantID {
				t.Errorf("Match() gotID = %v, want %v", gotID, tt.wantID)
			}
		})
	}
}
