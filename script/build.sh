#!/bin/bash

BIN_NAME="synctv"

function ChToScriptFileDir() {
    cd "$(dirname "$0")"
    if [ $? -ne 0 ]; then
        echo "cd to script file dir error"
        exit 1
    fi
}

function EnvHelp() {
    echo "SKIP_INIT_WEB"
    echo "WEB_VERSION set web dependency version (default: build version)"
    echo "DISABLE_TRIM_PATH enable trim path (default: disable)"
}

function Help() {
    echo "-h get help"
    echo "-v set build version (default: dev)"
    echo "-w init web version (default: build version)"
    echo "-s skip init web"
    echo "-S set source dir (default: ../)"
    echo "-m set build mode (default: pie)"
    echo "-l set ldflags (default: -s -w --extldflags \"-static -fpic -Wl,-z,relro,-z,now\")"
    echo "-p set platform (default: host platform, support: all, linux, darwin, windows)"
    echo "-P set disable trim path (default: disable)"
    echo "-d set build result dir (default: build)"
    echo "-T set tags (default: jsoniter)"
    echo "Env Help:"
    EnvHelp
}

function Init() {
    CGO_ENABLED=0
    VERSION="dev"
    commit="$(git log --pretty=format:"%h" -1)"
    if [ $? -ne 0 ]; then
        echo "git log error"
        GIT_COMMIT="unknown"
    else
        GIT_COMMIT="$commit"
    fi
    BUILD_MODE="pie"
    LDFLAGS="-s -w --extldflags '-static -fpic -Wl,-z,relro,-z,now'"
    PLATFORM=""
    BUILD_DIR="../build"
    TAGS="jsoniter"
    SOURCH_DIR="../"
}

function ParseArgs() {
    while getopts "hsS:v:w:m:l:p:Pd:T:" arg; do
        case $arg in
        h)
            Help
            exit 0
            ;;
        v)
            VERSION="$(echo "$OPTARG" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
            ;;
        s)
            SKIP_INIT_WEB="true"
            ;;
        S)
            SOURCH_DIR="$OPTARG"
            ;;
        w)
            WEB_VERSION="$OPTARG"
            ;;
        m)
            BUILD_MODE="$OPTARG"
            ;;
        l)
            LDFLAGS="$OPTARG"
            ;;
        p)
            PLATFORM="$OPTARG"
            ;;
        P)
            DISABLE_TRIM_PATH="true"
            ;;
        d)
            BUILD_DIR="$OPTARG"
            ;;
        T)
            TAGS="$OPTARG"
            ;;
        ?)
            echo "unkonw argument"
            exit 1
            ;;
        esac
    done
}

# Comply with golang version rules
function CheckVersionFormat() {
    if [ "$1" == "dev" ] || [ "$(echo "$1" | grep -oE "^v?[0-9]+\.[0-9]+\.[0-9]+(\-beta.*|\-rc.*|\-alpha.*)?$")" ]; then
        return 0
    else
        echo "version format error: $1"
        exit 1
    fi
}

function FixArgs() {
    CheckAllPlatform
    CheckVersionFormat "$VERSION"
    if [ ! "$SKIP_INIT_WEB" ] && [ ! "$WEB_VERSION" ]; then
        WEB_VERSION="$VERSION"
    fi
    LDFLAGS="$LDFLAGS \
        -X 'github.com/synctv-org/synctv/internal/version.Version=$VERSION' \
        -X 'github.com/synctv-org/synctv/internal/version.WebVersion=$WEB_VERSION' \
        -X 'github.com/synctv-org/synctv/internal/version.GitCommit=$GIT_COMMIT'"

    # trim / at the end
    BUILD_DIR="$(echo "$BUILD_DIR" | sed 's#/$##')"
    if [ ! "$SOURCH_DIR" ] || [ ! -d "$SOURCH_DIR" ]; then
        echo "source dir error: $SOURCH_DIR"
        exit 1
    fi
    echo "build source dir: $SOURCH_DIR"
}

function InitDep() {
    if [ "$SKIP_INIT_WEB" ]; then
        echo "skip init web"
        return
    fi
    rm -rf "../public/dist/*"
    echo "download: https://github.com/synctv-org/synctv-web/releases/download/${WEB_VERSION}/dist.tar.gz"
    curl -sL "https://github.com/synctv-org/synctv-web/releases/download/${WEB_VERSION}/dist.tar.gz" | tar --strip-components 1 -C "../public/dist" -z -x -v -f -
    if [ $? -ne 0 ]; then
        echo "download web error"
        exit 1
    fi
}

# https://go.dev/doc/install/source#environment
# $GOOS	$GOARCH
# aix	ppc64
# android	386
# android	amd64
# android	arm
# android	arm64
# darwin	amd64
# darwin	arm64
# dragonfly	amd64
# freebsd	386
# freebsd	amd64
# freebsd	arm
# illumos	amd64
# ios	arm64
# js	wasm
# linux	386
# linux	amd64
# linux	arm
# linux	arm64
# linux	loong64
# linux	mips
# linux	mipsle
# linux	mips64
# linux	mips64le
# linux	ppc64
# linux	ppc64le
# linux	riscv64
# linux	s390x
# netbsd	386
# netbsd	amd64
# netbsd	arm
# openbsd	386
# openbsd	amd64
# openbsd	arm
# openbsd	arm64
# plan9	386
# plan9	amd64
# plan9 arm
# solaris	amd64
# wasip1	wasm
# windows	386
# windows	amd64
# windows	arm
# windows   arm64

# sqlite3 not support linux/loong64,linux/mips linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64,
LINUX_ALLOWED_PLATFORM="linux/386,linux/amd64,linux/arm,linux/arm64,linux/ppc64le,linux/riscv64,linux/s390x"

DARWIN_ALLOWED_PLATFORM="darwin/amd64,darwin/arm64"

# sqlite3 not support windows/arm,windows/386
WINDOWS_ALLOWED_PLATFORM="windows/amd64,windows/arm64"

ALLOWED_PLATFORM="$LINUX_ALLOWED_PLATFORM,$DARWIN_ALLOWED_PLATFORM,$WINDOWS_ALLOWED_PLATFORM"

function CheckPlatform() {
    platform="$1"
    for p in $(echo "$ALLOWED_PLATFORM" | tr "," "\n"); do
        if [ "$p" == "$platform" ]; then
            return 0
        fi
    done
    return 1
}

function CheckAllPlatform() {
    for platform in $(echo "$PLATFORM" | tr "," "\n"); do
        if [ "$platform" == "all" ]; then
            PLATFORM="all"
            return 0
        elif [ "$platform" == "linux" ]; then
            continue
        elif [ "$platform" == "darwin" ]; then
            continue
        elif [ "$platform" == "windows" ]; then
            continue
        fi
        CheckPlatform "$platform"
        if [ $? -ne 0 ]; then
            echo "platform $platform not allowd"
            exit 1
        fi
    done
}

function Build() {
    platform="$1"
    echo "build $platform"
    GOOS=${platform%/*}
    GOARCH=${platform#*/}

    if [ "$GOOS" == "windows" ]; then
        EXT=".exe"
    else
        EXT=""
    fi

    BUILD_FLAGS="-tags \"$TAGS\" -ldflags \"$LDFLAGS\""
    BUILD_ENV="CGO_ENABLED=$CGO_ENABLED GOOS=$GOOS GOARCH=$GOARCH"
    if [ ! "$DISABLE_TRIM_PATH" ]; then
        BUILD_FLAGS="$BUILD_FLAGS -trimpath"
    fi

    # https://go.dev/doc/install/source#environment
    case "$GOARCH" in
    "386")
        # default sse2
        eval "$BUILD_ENV GO386=sse2 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH softfloat"
        eval "$BUILD_ENV GO386=softfloat go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-softfloat$EXT\" \"$SOURCH_DIR\""
        ;;
    "arm")
        # default v7
        # https://go.dev/wiki/GoArm
        eval "$BUILD_ENV GOARM=7 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH v5"
        eval "$BUILD_ENV GOARM=5 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-v5$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH v6"
        eval "$BUILD_ENV GOARM=6 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-v6$EXT\" \"$SOURCH_DIR\""
        ;;
    "amd64")
        # default v1
        # https://go.dev/wiki/MinimumRequirements#amd64
        eval "$BUILD_ENV GOAMD64=v1 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH v2"
        eval "$BUILD_ENV GOAMD64=v2 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-v2$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH v3"
        eval "$BUILD_ENV GOAMD64=v3 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-v3$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH v4"
        eval "$BUILD_ENV GOAMD64=v4 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-v4$EXT\" \"$SOURCH_DIR\""
        ;;
    "mips" | "mipsle" | "mips64" | "mips64le")
        # default hardfloat
        eval "$BUILD_ENV GOMIPS=hardfloat GOMIPS64=hardfloat go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH softfloat"
        eval "$BUILD_ENV GOMIPS=softfloat GOMIPS64=softfloat go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-softfloat$EXT\" \"$SOURCH_DIR\""
        ;;
    "ppc64" | "ppc64le")
        # default power8
        eval "$BUILD_ENV GOPPC64=power8 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH power9"
        eval "$BUILD_ENV GOPPC64=power9 go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-power9$EXT\" \"$SOURCH_DIR\""
        ;;
    "wasm")
        # no default
        eval "$BUILD_ENV go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH satconv"
        eval "$BUILD_ENV GOWASM=satconv go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-satconv$EXT\" \"$SOURCH_DIR\""
        echo "build $GOOS/$GOARCH signext"
        eval "$BUILD_ENV GOWASM=signext go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH-signext$EXT\" \"$SOURCH_DIR\""
        ;;
    *)
        eval "$BUILD_ENV go build $BUILD_FLAGS -o \"$BUILD_DIR/$BIN_NAME-$GOOS-$GOARCH$EXT\" \"$SOURCH_DIR\""
        ;;
    esac
}

function BuildHost() {
    GOOS="$(go env GOHOSTOS)"
    GOARCH="$(go env GOHOSTARCH)"
    if [ "$GOOS" == "windows" ]; then
        EXT=".exe"
    else
        EXT=""
    fi
    echo "build $GOOS/$GOARCH"
    if [ ! "$DISABLE_TRIM_PATH" ]; then
        CGO_ENABLED=$CGO_ENABLED GOOS=$GOOS GOARCH=$GOARCH go build -trimpath -tags "$TAGS" -ldflags "$LDFLAGS" -o "$BUILD_DIR/$BIN_NAME$EXT" "$SOURCH_DIR"
    else
        CGO_ENABLED=$CGO_ENABLED GOOS=$GOOS GOARCH=$GOARCH go build -tags "$TAGS" -ldflags "$LDFLAGS" -o "$BUILD_DIR/$BIN_NAME$EXT" "$SOURCH_DIR"
    fi
    if [ $? -ne 0 ]; then
        echo "build $GOOS/$GOARCH error"
        exit 1
    fi
}

function BuildAll() {
    if [ ! "$1" ]; then
        BuildHost
        return
    else
        for platform in $(echo "$1" | tr "," "\n"); do
            if [ "$platform" == "all" ]; then
                BuildAll "$ALLOWED_PLATFORM"
            elif [ "$platform" == "linux" ]; then
                BuildAll "$LINUX_ALLOWED_PLATFORM"
            elif [ "$platform" == "darwin" ]; then
                BuildAll "$DARWIN_ALLOWED_PLATFORM"
            elif [ "$platform" == "windows" ]; then
                BuildAll "$WINDOWS_ALLOWED_PLATFORM"
            else
                Build "$platform"
            fi
        done
    fi
}

ChToScriptFileDir
Init
ParseArgs "$@"
FixArgs
InitDep
BuildAll "$PLATFORM"
