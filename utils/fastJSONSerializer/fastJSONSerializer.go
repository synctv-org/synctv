package fastjsonserializer

import (
	"context"
	"fmt"
	"reflect"

	jsoniter "github.com/json-iterator/go"

	"gorm.io/gorm/schema"
)

var json = jsoniter.ConfigCompatibleWithStandardLibrary

type JSONSerializer struct{}

func (*JSONSerializer) Scan(ctx context.Context, field *schema.Field, dst reflect.Value, dbValue interface{}) (err error) {
	fieldValue := reflect.New(field.FieldType)

	if dbValue != nil {
		var bytes []byte
		switch v := dbValue.(type) {
		case []byte:
			bytes = v
		case string:
			bytes = []byte(v)
		default:
			return fmt.Errorf("failed to unmarshal JSONB value: %#v", dbValue)
		}

		err = json.Unmarshal(bytes, fieldValue.Interface())
	}

	field.ReflectValueOf(ctx, dst).Set(fieldValue.Elem())
	return
}

// 实现 Value 方法
func (*JSONSerializer) Value(ctx context.Context, field *schema.Field, dst reflect.Value, fieldValue interface{}) (interface{}, error) {
	return json.Marshal(fieldValue)
}

func init() {
	schema.RegisterSerializer("fastjson", new(JSONSerializer))
}
