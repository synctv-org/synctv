package utils_test

import (
	"reflect"
	"testing"

	"github.com/synctv-org/synctv/utils"
)

func TestGetPageItems(t *testing.T) {
	type args struct {
		items []int
		max   int64
		page  int64
	}
	tests := []struct {
		name string
		args args
		want []int
	}{
		{
			name: "Test Case 1",
			args: args{
				items: []int{1, 2, 3, 4, 5, 6, 7, 8, 9, 10},
				max:   5,
				page:  1,
			},
			want: []int{1, 2, 3, 4, 5},
		},
		{
			name: "Test Case 2",
			args: args{
				items: []int{1, 2, 3, 4, 5, 6, 7, 8, 9, 10},
				max:   5,
				page:  2,
			},
			want: []int{6, 7, 8, 9, 10},
		},
		{
			name: "Test Case 3",
			args: args{
				items: []int{1, 2, 3, 4, 5, 6, 7, 8, 9, 10},
				max:   5,
				page:  3,
			},
			want: []int{},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := utils.GetPageItems(tt.args.items, tt.args.max, tt.args.page); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("GetPageItems() = %v, want %v", got, tt.want)
			}
		})
	}
}

func FuzzCompVersion(f *testing.F) {
	f.Add("v1.0.0", "v1.0.1")
	f.Add("v0.2.9", "v1.5.2")
	f.Fuzz(func(t *testing.T, a, b string) {
		t.Logf("a: %s, b: %s", a, b)
		_, err := utils.CompVersion(a, b)
		if err != nil {
			t.Errorf("CompVersion error = %v", err)
		}
	})
}
