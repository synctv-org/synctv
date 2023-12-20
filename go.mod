module github.com/synctv-org/synctv

go 1.21

require (
	github.com/caarlos0/env/v9 v9.0.0
	github.com/cavaliergopher/grab/v3 v3.0.1
	github.com/gin-contrib/cors v1.5.0
	github.com/gin-gonic/gin v1.9.1
	github.com/glebarez/sqlite v1.10.0
	github.com/go-kratos/aegis v0.2.0
	github.com/go-kratos/kratos/contrib/registry/consul/v2 v2.0.0-20231216092143-856bc9a17b4a
	github.com/go-kratos/kratos/contrib/registry/etcd/v2 v2.0.0-20231216092143-856bc9a17b4a
	github.com/go-kratos/kratos/v2 v2.7.2
	github.com/go-resty/resty/v2 v2.10.0
	github.com/golang-jwt/jwt/v4 v4.5.0
	github.com/golang-jwt/jwt/v5 v5.2.0
	github.com/google/go-github/v56 v56.0.0
	github.com/google/uuid v1.5.0
	github.com/gorilla/websocket v1.5.1
	github.com/hashicorp/consul/api v1.26.1
	github.com/hashicorp/go-hclog v1.6.2
	github.com/hashicorp/go-plugin v1.6.0
	github.com/json-iterator/go v1.1.12
	github.com/maruel/natural v1.1.1
	github.com/mitchellh/go-homedir v1.1.0
	github.com/natefinch/lumberjack v2.0.0+incompatible
	github.com/quic-go/quic-go v0.40.1
	github.com/sirupsen/logrus v1.9.3
	github.com/soheilhy/cmux v0.1.5
	github.com/spf13/cobra v1.8.0
	github.com/synctv-org/vendors v0.1.1-0.20231219160707-b86154001009
	github.com/ulule/limiter/v3 v3.11.2
	github.com/zencoder/go-dash/v3 v3.0.3
	github.com/zijiren233/gencontainer v0.0.0-20231213075414-f7f4c8261dca
	github.com/zijiren233/go-colorable v0.0.0-20230930131441-997304c961cb
	github.com/zijiren233/livelib v0.3.0
	github.com/zijiren233/stream v0.5.1
	github.com/zijiren233/yaml-comment v0.2.1
	go.etcd.io/etcd/client/v3 v3.5.11
	golang.org/x/crypto v0.17.0
	golang.org/x/exp v0.0.0-20231214170342-aacd6d4b4611
	golang.org/x/oauth2 v0.15.0
	google.golang.org/grpc v1.60.0
	google.golang.org/protobuf v1.31.0
	gopkg.in/yaml.v3 v3.0.1
	gorm.io/driver/mysql v1.5.2
	gorm.io/driver/postgres v1.5.4
	gorm.io/gorm v1.25.5
)

require (
	cloud.google.com/go/compute v1.23.3 // indirect
	cloud.google.com/go/compute/metadata v0.2.3 // indirect
	github.com/BurntSushi/toml v1.3.2 // indirect
	github.com/armon/go-metrics v0.4.1 // indirect
	github.com/bytedance/sonic v1.10.2 // indirect
	github.com/chenzhuoyu/base64x v0.0.0-20230717121745-296ad89f973d // indirect
	github.com/chenzhuoyu/iasm v0.9.1 // indirect
	github.com/coreos/go-semver v0.3.1 // indirect
	github.com/coreos/go-systemd/v22 v22.5.0 // indirect
	github.com/dustin/go-humanize v1.0.1 // indirect
	github.com/fatih/color v1.16.0 // indirect
	github.com/gabriel-vasile/mimetype v1.4.3 // indirect
	github.com/gin-contrib/sse v0.1.0 // indirect
	github.com/glebarez/go-sqlite v1.21.2 // indirect
	github.com/go-playground/form/v4 v4.2.1 // indirect
	github.com/go-playground/locales v0.14.1 // indirect
	github.com/go-playground/universal-translator v0.18.1 // indirect
	github.com/go-playground/validator/v10 v10.16.0 // indirect
	github.com/go-sql-driver/mysql v1.7.1 // indirect
	github.com/go-task/slim-sprig v0.0.0-20230315185526-52ccab3ef572 // indirect
	github.com/goccy/go-json v0.10.2 // indirect
	github.com/gogo/protobuf v1.3.2 // indirect
	github.com/golang/protobuf v1.5.3 // indirect
	github.com/google/go-querystring v1.1.0 // indirect
	github.com/google/pprof v0.0.0-20231212022811-ec68065c825e // indirect
	github.com/google/wire v0.5.0 // indirect
	github.com/gorilla/mux v1.8.1 // indirect
	github.com/hashicorp/errwrap v1.1.0 // indirect
	github.com/hashicorp/go-cleanhttp v0.5.2 // indirect
	github.com/hashicorp/go-immutable-radix v1.3.1 // indirect
	github.com/hashicorp/go-multierror v1.1.1 // indirect
	github.com/hashicorp/go-rootcerts v1.0.2 // indirect
	github.com/hashicorp/golang-lru v1.0.2 // indirect
	github.com/hashicorp/serf v0.10.1 // indirect
	github.com/hashicorp/yamux v0.1.1 // indirect
	github.com/inconshreveable/mousetrap v1.1.0 // indirect
	github.com/jackc/pgpassfile v1.0.0 // indirect
	github.com/jackc/pgservicefile v0.0.0-20231201235250-de7065d80cb9 // indirect
	github.com/jackc/pgx/v5 v5.5.1 // indirect
	github.com/jackc/puddle/v2 v2.2.1 // indirect
	github.com/jinzhu/inflection v1.0.0 // indirect
	github.com/jinzhu/now v1.1.5 // indirect
	github.com/klauspost/cpuid/v2 v2.2.6 // indirect
	github.com/leodido/go-urn v1.2.4 // indirect
	github.com/mattn/go-colorable v0.1.13 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/mitchellh/go-testing-interface v1.14.1 // indirect
	github.com/mitchellh/mapstructure v1.5.0 // indirect
	github.com/modern-go/concurrent v0.0.0-20180306012644-bacd9c7ef1dd // indirect
	github.com/modern-go/reflect2 v1.0.2 // indirect
	github.com/oklog/run v1.1.0 // indirect
	github.com/onsi/ginkgo/v2 v2.13.2 // indirect
	github.com/pelletier/go-toml/v2 v2.1.1 // indirect
	github.com/pkg/errors v0.9.1 // indirect
	github.com/quic-go/qpack v0.4.0 // indirect
	github.com/quic-go/qtls-go1-20 v0.4.1 // indirect
	github.com/remyoudompheng/bigfft v0.0.0-20230129092748-24d4a6f8daec // indirect
	github.com/spf13/pflag v1.0.5 // indirect
	github.com/twitchyliquid64/golang-asm v0.15.1 // indirect
	github.com/ugorji/go/codec v1.2.12 // indirect
	go.etcd.io/etcd/api/v3 v3.5.11 // indirect
	go.etcd.io/etcd/client/pkg/v3 v3.5.11 // indirect
	go.uber.org/mock v0.3.0 // indirect
	go.uber.org/multierr v1.11.0 // indirect
	go.uber.org/zap v1.26.0 // indirect
	golang.org/x/arch v0.6.0 // indirect
	golang.org/x/mod v0.14.0 // indirect
	golang.org/x/net v0.19.0 // indirect
	golang.org/x/sync v0.5.0 // indirect
	golang.org/x/sys v0.15.0 // indirect
	golang.org/x/text v0.14.0 // indirect
	golang.org/x/tools v0.16.1 // indirect
	google.golang.org/appengine v1.6.8 // indirect
	google.golang.org/genproto v0.0.0-20231212172506-995d672761c0 // indirect
	google.golang.org/genproto/googleapis/api v0.0.0-20231212172506-995d672761c0 // indirect
	google.golang.org/genproto/googleapis/rpc v0.0.0-20231212172506-995d672761c0 // indirect
	gopkg.in/natefinch/lumberjack.v2 v2.2.1 // indirect
	modernc.org/libc v1.37.6 // indirect
	modernc.org/mathutil v1.6.0 // indirect
	modernc.org/memory v1.7.2 // indirect
	modernc.org/sqlite v1.28.0 // indirect
)
