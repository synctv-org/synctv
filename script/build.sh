#!/bin/bash
set -e

# 使用更丰富的颜色定义
readonly COLOR_RED='\033[0;31m'
readonly COLOR_GREEN='\033[0;32m'
readonly COLOR_YELLOW='\033[0;33m'
readonly COLOR_BLUE='\033[0;34m'
readonly COLOR_PURPLE='\033[0;35m'
readonly COLOR_CYAN='\033[0;36m'
readonly COLOR_LIGHT_GRAY='\033[0;37m'
readonly COLOR_DARK_GRAY='\033[1;30m'
readonly COLOR_LIGHT_RED='\033[1;31m'
readonly COLOR_LIGHT_GREEN='\033[1;32m'
readonly COLOR_RESET='\033[0m'

readonly DEFAULT_SOURCE_DIR="$(pwd)"
readonly DEFAULT_RESULT_DIR="${DEFAULT_SOURCE_DIR}/build"
readonly DEFAULT_BUILD_CONFIG="${DEFAULT_SOURCE_DIR}/build.config.sh"
readonly DEFAULT_CGO_ENABLED="1"
readonly DEFAULT_CC="gcc"
readonly DEFAULT_CXX="g++"
readonly DEFAULT_CGO_CROSS_COMPILER_DIR="${DEFAULT_SOURCE_DIR}/cross"
readonly DEFAULT_CGO_FLAGS="-O2 -g0 -pipe"
readonly DEFAULT_CGO_LDFLAGS="-s"
readonly DEFAULT_LDFLAGS="-s -w"
readonly DEFAULT_CGO_DEPS_VERSION="v0.4.6"
readonly DEFAULT_TTY_WIDTH="40"

readonly GOHOSTOS="$(go env GOHOSTOS)"
readonly GOHOSTARCH="$(go env GOHOSTARCH)"
readonly GOHOSTPLATFORM="${GOHOSTOS}/${GOHOSTARCH}"

function printBuildConfigHelp() {
    echo -e "${COLOR_YELLOW}you can customize build config${COLOR_RESET} (default: ${DEFAULT_BUILD_CONFIG})"
    echo -e "${COLOR_LIGHT_GREEN}parseDepArgs${COLOR_RESET} parse dep args"
    echo -e "${COLOR_LIGHT_GREEN}printDepHelp${COLOR_RESET} print dep help"
    echo -e "${COLOR_LIGHT_GREEN}printDepEnvHelp${COLOR_RESET} print dep env help"
    echo -e "${COLOR_LIGHT_GREEN}initDepPlatforms${COLOR_RESET} init dep platforms"
    echo -e "${COLOR_LIGHT_GREEN}initDep${COLOR_RESET} init dep"
}

function printEnvHelp() {
    echo -e "${COLOR_CYAN}SOURCE_DIR${COLOR_RESET} set source dir (default: ${DEFAULT_SOURCE_DIR})"
    echo -e "${COLOR_CYAN}RESULT_DIR${COLOR_RESET} set build result dir (default: ${DEFAULT_RESULT_DIR})"
    echo -e "${COLOR_CYAN}BUILD_CONFIG${COLOR_RESET} set build config (default: ${DEFAULT_BUILD_CONFIG})"
    echo -e "${COLOR_CYAN}BIN_NAME${COLOR_RESET} set bin name (default: ${source_dir} basename)"
    echo -e "${COLOR_CYAN}PLATFORM${COLOR_RESET} set platform (default: host platform, support: all, linux, linux/arm*, ...)"
    echo -e "${COLOR_CYAN}DISABLE_MICRO${COLOR_RESET} set will not build micro"
    echo -e "${COLOR_CYAN}CGO_ENABLED${COLOR_RESET} set cgo enabled (default: ${DEFAULT_CGO_ENABLED})"
    echo -e "${COLOR_CYAN}HOST_CC${COLOR_RESET} set host cc (default: ${DEFAULT_CC})"
    echo -e "${COLOR_CYAN}HOST_CXX${COLOR_RESET} set host cxx (default: ${DEFAULT_CXX})"
    echo -e "${COLOR_CYAN}FORCE_CC${COLOR_RESET} set force gcc"
    echo -e "${COLOR_CYAN}FORCE_CXX${COLOR_RESET} set force g++"
    echo -e "${COLOR_CYAN}*_ALLOWED_PLATFORM${COLOR_RESET} set allowed platform (example: LINUX_ALLOWED_PLATFORM=\"linux/amd64\")"
    echo -e "${COLOR_CYAN}CGO_*_ALLOWED_PLATFORM${COLOR_RESET} set cgo allowed platform (example: CGO_LINUX_ALLOWED_PLATFORM=\"linux/amd64\")"
    echo -e "${COLOR_CYAN}GH_PROXY${COLOR_RESET} set github proxy releases mirror (example: https://mirror.ghproxy.com/)"

    if declare -f printDepEnvHelp >/dev/null; then
        echo -e "${COLOR_LIGHT_GRAY}$(getSeparator)${COLOR_RESET}"
        echo -e "Dep Env:"
        printDepEnvHelp
    fi
}

function printHelp() {
    echo -e "${COLOR_BLUE}-h, --help${COLOR_RESET} get help"
    echo -e "${COLOR_BLUE}--disable-cgo${COLOR_RESET} disable cgo"
    echo -e "${COLOR_BLUE}--source-dir=${COLOR_RESET} set source dir (default: ${DEFAULT_SOURCE_DIR})"
    echo -e "${COLOR_BLUE}--more-go-cmd-args=${COLOR_RESET} more go cmd args"
    echo -e "${COLOR_BLUE}--disable-micro${COLOR_RESET} disable build micro"
    echo -e "${COLOR_BLUE}--ldflags=${COLOR_RESET} set ldflags (default: \"${DEFAULT_LDFLAGS}\")"
    echo -e "${COLOR_BLUE}--platforms=${COLOR_RESET} set platforms (default: host platform, support: all, linux, linux/arm*, ...)"
    echo -e "${COLOR_BLUE}--result-dir=${COLOR_RESET} set build result dir (default: ${DEFAULT_RESULT_DIR})"
    echo -e "${COLOR_BLUE}--tags=${COLOR_RESET} set tags"
    echo -e "${COLOR_BLUE}--show-all-targets${COLOR_RESET} show all targets"
    echo -e "${COLOR_BLUE}--github-proxy-mirror=${COLOR_RESET} use github proxy mirror"
    echo -e "${COLOR_BLUE}--force-gcc=${COLOR_RESET} set force gcc"
    echo -e "${COLOR_BLUE}--force-g++=${COLOR_RESET} set force g++"
    echo -e "${COLOR_BLUE}--host-cc=${COLOR_RESET} host cc (default: ${DEFAULT_CC})"
    echo -e "${COLOR_BLUE}--host-cxx=${COLOR_RESET} host cxx (default: ${DEFAULT_CXX})"

    echo -e "${COLOR_DARK_GRAY}$(getSeparator)${COLOR_RESET}"
    printBuildConfigHelp

    if declare -f printDepHelp >/dev/null; then
        echo -e "${COLOR_PURPLE}$(getSeparator)${COLOR_RESET}"
        echo -e "Dep Help:"
        printDepHelp
    fi

    echo -e "${COLOR_LIGHT_GRAY}$(getSeparator)${COLOR_RESET}"
    echo -e "Env Help:"
    printEnvHelp
}

function setDefault() {
    local var_name="$1"
    local default_value="$2"
    [[ -z "${!var_name}" ]] && eval "${var_name}=\"${default_value}\""
    return 0
}

function addTags() {
    local new_tags="$1"
    [[ -n "${new_tags}" ]] && tags="${tags} ${new_tags}"
}

function addLDFLAGS() {
    local new_ldflags="$1"
    [[ -n "${new_ldflags}" ]] && ldflags="${ldflags} ${new_ldflags}"
}

function addBuildArgs() {
    [[ -n "${1}" ]] && build_args="${build_args} ${1}"
}

function fixArgs() {
    setDefault "source_dir" "${DEFAULT_SOURCE_DIR}"
    source_dir="$(cd "${source_dir}" && pwd)"
    setDefault "bin_name" "$(basename "${source_dir}")"
    setDefault "result_dir" "${DEFAULT_RESULT_DIR}"
    mkdir -p "${result_dir}"
    result_dir="$(cd "${result_dir}" && pwd)"
    echo -e "${COLOR_BLUE}build source dir: ${COLOR_GREEN}${source_dir}${COLOR_RESET}"
    echo -e "${COLOR_BLUE}build result dir: ${COLOR_GREEN}${result_dir}${COLOR_RESET}"

    setDefault "cgo_cross_compiler_dir" "$DEFAULT_CGO_CROSS_COMPILER_DIR"
    mkdir -p "${cgo_cross_compiler_dir}"
    cgo_cross_compiler_dir="$(cd "${cgo_cross_compiler_dir}" && pwd)"

    setDefault "platforms" "${GOHOSTPLATFORM}"
    setDefault "disable_micro" ""
    setDefault "host_cc" "${DEFAULT_CC}"
    setDefault "host_cxx" "${DEFAULT_CXX}"
    setDefault "force_cc" ""
    setDefault "force_cxx" ""
    setDefault "gh_proxy" ""
    setDefault "tags" ""
    setDefault "ldflags" "${DEFAULT_LDFLAGS}"
    setDefault "build_args" ""
    setDefault "cgo_deps_version" "${DEFAULT_CGO_DEPS_VERSION}"
}

function isCGOEnabled() {
    [[ "${cgo_enabled}" == "1" ]]
}

function downloadAndUnzip() {
    local url="$1"
    local file="$2"
    local type="${3:-$(echo "${url}" | sed 's/.*\.//g')}"

    mkdir -p "${file}"
    file="$(cd "${file}" && pwd)"
    echo -e "${COLOR_BLUE}download ${COLOR_CYAN}\"${url}\"${COLOR_BLUE} to ${COLOR_GREEN}\"${file}\"${COLOR_RESET}"
    rm -rf "${file}"/*

    local start_time=$(date +%s)

    case "${type}" in
    "tgz" | "gz")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -z
        ;;
    "bz2")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -j
        ;;
    "xz")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -J
        ;;
    "lzma")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 --lzma
        ;;
    "zip")
        curl -sL "${url}" -o "${file}/tmp.zip"
        unzip -o "${file}/tmp.zip" -d "${file}" -q
        rm -f "${file}/tmp.zip"
        ;;
    *)
        echo -e "${COLOR_RED}compress type: ${type} not support${COLOR_RESET}"
        return 1
        ;;
    esac

    local end_time=$(date +%s)
    echo -e "${COLOR_GREEN}download and unzip success: $((end_time - start_time))s${COLOR_RESET}"
}

declare -A allowed_platforms
allowed_platforms=(
    ["linux"]="linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64,linux/ppc64le,linux/riscv64,linux/s390x"
    ["darwin"]="darwin/amd64,darwin/arm64"
    ["windows"]="windows/386,windows/amd64,windows/arm,windows/arm64"
)

declare -A cgo_allowed_platforms
cgo_allowed_platforms=(
    ["linux"]="linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64le,linux/riscv64,linux/s390x"
    ["windows"]="windows/386,windows/amd64"
)

function addToAllowedPlatforms() {
    local platforms="$1"
    local platform=""
    for platform in ${platform//,/ }; do
        local os="${platform%%/*}"
        if [[ -z "${allowed_platforms[${os}]}" ]]; then
            allowed_platforms[${os}]="${platform}"
        else
            allowed_platforms[${os}]="${allowed_platforms[${os}]},${platform}"
        fi
    done
}

function addToCGOAllowedPlatforms() {
    local platforms="$1"
    local platform=""
    for platform in ${platforms//,/ }; do
        local os="${platform%%/*}"
        if [[ -z "${cgo_allowed_platforms[${os}]}" ]]; then
            cgo_allowed_platforms[${os}]="${platform}"
        else
            cgo_allowed_platforms[${os}]="${cgo_allowed_platforms[${os}]},${platform}"
        fi
    done
}

function deleteFromAllowedPlatforms() {
    local platforms="$1"
    local platform=""
    for platform in ${platforms//,/ }; do
        local os="${platform%%/*}"
        if [[ -n "${allowed_platforms[${os}]}" ]]; then
            allowed_platforms[${os}]=$(echo "${allowed_platforms[${os}]}" | sed "s|${platform}$||g" | sed "s|${platform},||g")
        fi
    done
}

function deleteFromCGOAllowedPlatforms() {
    local platforms="$1"
    local platform=""
    for platform in ${platform//,/ }; do
        local os="${platform%%/*}"
        if [[ -n "${cgo_allowed_platforms[${os}]}" ]]; then
            cgo_allowed_platforms[${os}]=$(echo "${cgo_allowed_platforms[${os}]}" | sed "s|${platform}$||g" | sed "s|${platform},||g")
        fi
    done
}

function initHostPlatforms() {
    addToAllowedPlatforms "${GOHOSTOS}/${GOHOSTARCH}"
    addToCGOAllowedPlatforms "${GOHOSTOS}/${GOHOSTARCH}"
}

function removeDuplicatePlatforms() {
    local all_platforms="$1"
    all_platforms="$(echo "${all_platforms}" | tr ',' '\n' | sort | uniq | paste -s -d ',' -)"
    all_platforms="${all_platforms#,}"
    all_platforms="${all_platforms%,}"
    echo "${all_platforms}"
}

function initPlatforms() {
    setDefault "cgo_enabled" "${DEFAULT_CGO_ENABLED}"

    unset -v CURRENT_ALLOWED_PLATFORM

    ALLOWED_PLATFORM=""
    CGO_ALLOWED_PLATFORM=""
    for os in "${!allowed_platforms[@]}"; do
        ALLOWED_PLATFORM="${ALLOWED_PLATFORM},${allowed_platforms[${os}]}"
        if [[ -n "${cgo_allowed_platforms[${os}]}" ]]; then
            CGO_ALLOWED_PLATFORM="${CGO_ALLOWED_PLATFORM},${cgo_allowed_platforms[${os}]}"
        fi
    done

    ALLOWED_PLATFORM=$(removeDuplicatePlatforms "${ALLOWED_PLATFORM}")
    CGO_ALLOWED_PLATFORM=$(removeDuplicatePlatforms "${CGO_ALLOWED_PLATFORM}")

    isCGOEnabled && CURRENT_ALLOWED_PLATFORM="${CGO_ALLOWED_PLATFORM}" || CURRENT_ALLOWED_PLATFORM="${ALLOWED_PLATFORM}"

    for os in "${!allowed_platforms[@]}"; do
        local var="${os^^}_ALLOWED_PLATFORM"
        local cgo_var="CGO_${var}"
        eval "CURRENT_${var}=\"${!var}\""
        eval "CURRENT_${cgo_var}=\"${!cgo_var}\""
    done

    if declare -f initDepPlatforms >/dev/null; then
        initDepPlatforms
    fi
}

function checkPlatform() {
    local target_platform="$1"
    local current_allowed_platform="${2:-${CURRENT_ALLOWED_PLATFORM}}"

    if [[ "${current_allowed_platform}" =~ (^|,)${target_platform}($|,) ]]; then
        echo "0"
    elif isCGOEnabled && [[ "${ALLOWED_PLATFORM}" =~ (^|,)${target_platform}($|,) ]]; then
        echo "2"
    else
        echo "1"
    fi
}

function checkPlatforms() {
    local platforms="$1"

    for platform in ${platforms//,/ }; do
        case $(checkPlatform "${platform}") in
        0)
            return 0
            ;;
        1)
            echo -e "${COLOR_RED}platform: ${platform} not support${COLOR_RESET}"
            return 1
            ;;
        2)
            echo -e "${COLOR_RED}platform: ${platform} not support for cgo${COLOR_RESET}"
            return 2
            ;;
        *)
            echo -e "${COLOR_RED}check platform error: ${platform}${COLOR_RESET}"
            return 3
            ;;
        esac
    done
}

declare -A cgo_deps
cgo_deps=(
    ["CC"]=""
    ["CXX"]=""
    ["MORE_CGO_CFLAGS"]=""
    ["MORE_CGO_CXXFLAGS"]=""
    ["MORE_CGO_LDFLAGS"]=""
)

function initCGODeps() {
    local goos="$1"
    local goarch="$2"
    local micro="$3"

    if [[ -n "${force_cc}" ]] && [[ -n "${force_cxx}" ]]; then
        cgo_deps["CC"]="${force_cc}"
        cgo_deps["CXX"]="${force_cxx}"
        return
    elif [[ -n "${force_cc}" ]] || [[ -n "${force_cxx}" ]]; then
        echo -e "${COLOR_RED}FORCE_CC and FORCE_CXX must be set at the same time${COLOR_RESET}"
        return 1
    fi

    if ! isCGOEnabled; then
        echo -e "${COLOR_RED}try use cgo, but cgo not enabled${COLOR_RESET}"
        return 1
    fi

    case "${GOHOSTOS}" in
    "linux" | "darwin")
        case "${GOHOSTARCH}" in
        "amd64" | "arm64" | "arm" | "ppc64le" | "riscv64" | "s390x")
            initDefaultCGODeps "$@"
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
            else
                echo -e "${COLOR_LIGHT_RED}${goos}/${goarch} not support for cgo${COLOR_RESET}"
                return 1
            fi
            ;;
        esac
        ;;
    *)
        if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
            initHostCGODeps "$@"
        else
            echo -e "${COLOR_RED}${goos}/${goarch} not support for cgo${COLOR_RESET}"
            return 1
        fi
        ;;
    esac

    local cc_command cc_options
    read -r cc_command cc_options <<<"${cgo_deps["CC"]}"
    cc_command="$(command -v "${cc_command}")"
    if [[ "${cc_command}" != /* ]]; then
        cgo_deps["CC"]="$(cd "$(dirname "${cc_command}")" && pwd)/$(basename "${cc_command}")"
        [[ -n "${cc_options}" ]] && cgo_deps["CC"]="${cgo_deps["CC"]} ${cc_options}"
    fi

    local cxx_command cxx_options
    read -r cxx_command cxx_options <<<"${cgo_deps["CXX"]}"
    cxx_command="$(command -v "${cxx_command}")"
    if [[ "${cxx_command}" != /* ]]; then
        cgo_deps["CXX"]="$(cd "$(dirname "${cxx_command}")" && pwd)/$(basename "${cxx_command}")"
        [[ -n "${cxx_options}" ]] && cgo_deps["CXX"]="${cgo_deps["CXX"]} ${cxx_options}"
    fi
}

function initHostCGODeps() {
    cgo_deps["CC"]="${host_cc}"
    cgo_deps["CXX"]="${host_cxx}"
}

function initDefaultCGODeps() {
    local goos="$1"
    local goarch="$2"
    local micro="$3"
    local unamespacer="${GOHOSTOS}-${GOHOSTARCH}"
    [[ "${GOHOSTARCH}" == "arm" ]] && unamespacer="${GOHOSTOS}-arm32v7"

    # 根据不同的目标平台和架构初始化 CGO 依赖项
    case "${goos}" in
    "linux")
        case "${micro}" in
        "hardfloat")
            micro="hf"
            ;;
        "softfloat")
            micro="sf"
            ;;
        esac
        case "${goarch}" in
        "386")
            initLinuxCGO "i686" ""
            ;;
        "amd64")
            initLinuxCGO "x86_64" ""
            ;;
        "arm")
            [[ "${micro}" == "5" ]] && initLinuxCGO "armv5" "eabi" || initLinuxCGO "armv${micro}" "eabihf"
            ;;
        "arm64")
            initLinuxCGO "aarch64" ""
            ;;
        "mips")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips" "" "${micro}"
            ;;
        "mipsle")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mipsel" "" "${micro}"
            ;;
        "mips64")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips64" "" "${micro}"
            ;;
        "mips64le")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips64el" "" "${micro}"
            ;;
        "ppc64")
            initLinuxCGO "powerpc64" ""
            ;;
        "ppc64le")
            initLinuxCGO "powerpc64le" ""
            ;;
        "riscv64")
            initLinuxCGO "riscv64" ""
            ;;
        "s390x")
            initLinuxCGO "s390x" ""
            ;;
        "loong64")
            initLinuxCGO "loongarch64" ""
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
            else
                echo -e "${COLOR_RED}${goos}/${goarch} not support for cgo${COLOR_RESET}"
                return 1
            fi
            ;;
        esac
        ;;
    "windows")
        case "${goarch}" in
        "386")
            initWindowsCGO "i686"
            ;;
        "amd64")
            initWindowsCGO "x86_64"
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
            else
                echo -e "${COLOR_RED}${goos}/${goarch} not support for cgo${COLOR_RESET}"
                return 1
            fi
            ;;
        esac
        ;;
    *)
        if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
            initHostCGODeps "$@"
        else
            echo -e "${COLOR_RED}${goos}/${goarch} not support for cgo${COLOR_RESET}"
            return 1
        fi
        ;;
    esac
}

function initLinuxCGO() {
    local arch_prefix="$1"
    local abi="$2"
    local micro="$3"
    local cc_var="CC_LINUX_${arch_prefix^^}${abi^^}${micro^^}"
    local cxx_var="CXX_LINUX_${arch_prefix^^}${abi^^}${micro^^}"

    if [[ -z "${!cc_var}" ]] && [[ -z "${!cxx_var}" ]]; then
        local cross_compiler_name="${arch_prefix}-linux-musl${abi}${micro}-cross"
        if command -v "${arch_prefix}-linux-musl${abi}${micro}-gcc" >/dev/null 2>&1 &&
            command -v "${arch_prefix}-linux-musl${abi}${micro}-g++" >/dev/null 2>&1; then
            eval "${cc_var}=\"${arch_prefix}-linux-musl${abi}${micro}-gcc\""
            eval "${cxx_var}=\"${arch_prefix}-linux-musl${abi}${micro}-g++\""
        elif [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc" ]] &&
            [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++" ]]; then
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++\""
        else
            downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${cgo_deps_version}/${cross_compiler_name}-${unamespacer}.tgz" \
                "${cgo_cross_compiler_dir}/${cross_compiler_name}"
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++\""
        fi
    elif [[ -z "${!cc_var}" ]] || [[ -z "${!cxx_var}" ]]; then
        echo -e "${COLOR_RED}${cc_var} or ${cxx_var} not found${COLOR_RESET}"
        return 1
    fi

    cgo_deps["CC"]="${!cc_var} -static --static"
    cgo_deps["CXX"]="${!cxx_var} -static --static"
}

function initWindowsCGO() {
    local arch_prefix="$1"
    local cc_var="CC_WINDOWS_${arch_prefix^^}"
    local cxx_var="CXX_WINDOWS_${arch_prefix^^}"

    if [[ -z "${!cc_var}" ]] && [[ -z "${!cxx_var}" ]]; then
        local cross_compiler_name="${arch_prefix}-w64-mingw32-cross"
        if command -v "${arch_prefix}-w64-mingw32-gcc" >/dev/null 2>&1 &&
            command -v "${arch_prefix}-w64-mingw32-g++" >/dev/null 2>&1; then
            eval "${cc_var}=\"${arch_prefix}-w64-mingw32-gcc\""
            eval "${cxx_var}=\"${arch_prefix}-w64-mingw32-g++\""
        elif [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc" ]] &&
            [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++" ]]; then
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++\""
        else
            downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${cgo_deps_version}/${cross_compiler_name}-${unamespacer}.tgz" \
                "${cgo_cross_compiler_dir}/${cross_compiler_name}"
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++\""
        fi
    elif [[ -z "${!cc_var}" ]] || [[ -z "${!cxx_var}" ]]; then
        echo -e "${COLOR_RED}${cc_var} or ${cxx_var} not found${COLOR_RESET}"
        return 1
    fi

    cgo_deps["CC"]="${!cc_var} -static --static"
    cgo_deps["CXX"]="${!cxx_var} -static --static"
}

function supportPIE() {
    local platform="$1"
    [ ! $(isCGOEnabled) ] &&
        [[ "${platform}" != "linux/386" ]] &&
        [[ "${platform}" != "linux/arm" ]] &&
        [[ "${platform}" != "linux/loong64" ]] &&
        [[ "${platform}" != "linux/riscv64" ]] &&
        [[ "${platform}" != "linux/s390x" ]] ||
        return 1
    [[ "${platform}" != "linux/mips"* ]] &&
        [[ "${platform}" != "linux/ppc64" ]] &&
        [[ "${platform}" != "openbsd"* ]] &&
        [[ "${platform}" != "freebsd"* ]] &&
        [[ "${platform}" != "netbsd"* ]]
}

function getSeparator() {
    local width=$(tput cols 2>/dev/null || echo $DEFAULT_TTY_WIDTH)
    local separator=""
    for ((i = 0; i < width; i++)); do
        separator+="-"
    done
    echo $separator
}

# 构建目标平台
function buildTarget() {
    local platform="$1"
    local target_name="$2"
    local goos="${platform%/*}"
    local goarch="${platform#*/}"
    local ext=""
    [[ "${goos}" == "windows" ]] && ext=".exe"
    local target_file="${result_dir}/${target_name}-${goos}-${goarch}${ext}"
    local build_mode=""
    supportPIE "${platform}" && build_mode="-buildmode=pie"

    local build_env=(
        "CGO_ENABLED=${cgo_enabled}"
        "GOOS=${goos}"
        "GOARCH=${goarch}"
    )

    echo -e "${COLOR_LIGHT_GRAY}$(getSeparator)${COLOR_RESET}"

    buildTargetWithMicro "" "${build_env[@]}"

    if [ -n "${disable_micro}" ]; then
        return
    fi

    case "${goarch}" in
    "386")
        echo
        buildTargetWithMicro "sse2" "${build_env[@]}"
        echo
        buildTargetWithMicro "softfloat" "${build_env[@]}"
        ;;
    "arm")
        echo
        buildTargetWithMicro "5" "${build_env[@]}"
        echo
        buildTargetWithMicro "6" "${build_env[@]}"
        echo
        buildTargetWithMicro "7" "${build_env[@]}"
        ;;
    "amd64")
        echo
        buildTargetWithMicro "v1" "${build_env[@]}"
        echo
        buildTargetWithMicro "v2" "${build_env[@]}"
        echo
        buildTargetWithMicro "v3" "${build_env[@]}"
        echo
        buildTargetWithMicro "v4" "${build_env[@]}"
        ;;
    "mips" | "mipsle" | "mips64" | "mips64le")
        echo
        buildTargetWithMicro "hardfloat" "${build_env[@]}"
        echo
        buildTargetWithMicro "softfloat" "${build_env[@]}"
        ;;
    "ppc64" | "ppc64le")
        echo
        buildTargetWithMicro "power8" "${build_env[@]}"
        echo
        buildTargetWithMicro "power9" "${build_env[@]}"
        ;;
    "wasm")
        echo
        buildTargetWithMicro "satconv" "${build_env[@]}"
        echo
        buildTargetWithMicro "signext" "${build_env[@]}"
        ;;
    esac
}

# 构建特定微架构的目标平台
function buildTargetWithMicro() {
    local micro="$1"
    local build_env=("${@:2}")
    local goos="${platform%/*}"
    local goarch="${platform#*/}"
    local ext=""
    [[ "${goos}" == "windows" ]] && ext=".exe"
    local target_file="${result_dir}/${bin_name}-${goos}-${goarch}${micro:+"-$micro"}${ext}"
    local default_target_file="${result_dir}/${bin_name}-${goos}-${goarch}${ext}"

    case "${goarch}" in
    "386")
        build_env+=("GO386=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    "arm")
        build_env+=("GOARM=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro:-6}"
        ;;
    "amd64")
        build_env+=("GOAMD64=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    "mips" | "mipsle")
        build_env+=("GOMIPS=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro:-hardfloat}"
        ;;
    "mips64" | "mips64le")
        build_env+=("GOMIPS64=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro:-hardfloat}"
        ;;
    "ppc64" | "ppc64le")
        build_env+=("GOPPC64=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    "wasm")
        build_env+=("GOWASM=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    *)
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    esac

    if isCGOEnabled; then
        build_env+=("CGO_CFLAGS=${DEFAULT_CGO_FLAGS} ${cgo_deps["MORE_CGO_CFLAGS"]}")
        build_env+=("CGO_CXXFLAGS=${DEFAULT_CGO_FLAGS} ${cgo_deps["MORE_CGO_CXXFLAGS"]}")
        build_env+=("CGO_LDFLAGS=${DEFAULT_CGO_LDFLAGS} ${cgo_deps["MORE_CGO_LDFLAGS"]}")
        build_env+=("CC=${cgo_deps["CC"]}")
        build_env+=("CXX=${cgo_deps["CXX"]}")
    fi

    echo -e "${COLOR_PURPLE}building ${goos}/${goarch}${micro:+/${micro}}${COLOR_RESET}"
    echo "${build_env[@]}"
    env "${build_env[@]}" go build -tags "${tags}" -ldflags "${ldflags}" -trimpath ${build_args} ${build_mode} -o "${target_file}" "${source_dir}"
    echo -e "${COLOR_LIGHT_GREEN}build ${goos}/${goarch}${micro:+ ${micro}} success${COLOR_RESET}"
}

function expandPlatforms() {
    local platforms="$1"
    local expanded_platforms=""
    local platform=""
    for platform in ${platforms//,/ }; do
        if [[ "${platform}" == "all" ]]; then
            echo "${CURRENT_ALLOWED_PLATFORM}"
            return
        elif [[ "${platform}" == *\** ]]; then
            local tmp_var=""
            for tmp_var in ${CURRENT_ALLOWED_PLATFORM//,/ }; do
                [[ "${tmp_var}" == ${platform} ]] && expanded_platforms="${expanded_platforms} ${tmp_var}"
            done
        elif [[ "${platform}" != */* ]]; then
            expanded_platforms="${expanded_platforms} $(expandPlatforms "${platform}/*")"
        else
            expanded_platforms="${expanded_platforms} ${platform}"
        fi
    done
    removeDuplicatePlatforms "${expanded_platforms}"
}

function autoBuild() {
    local platforms=$(expandPlatforms "$1")
    checkPlatforms "${platforms}"

    if declare -f initDep >/dev/null; then
        initDep
    fi

    for platform in ${platforms//,/ }; do
        buildTarget "${platform}" "${bin_name}"
    done
}

function loadedBuildConfig() {
    if [[ -n "${load_build_config}" ]]; then
        return 0
    fi
    return 1
}

function loadBuildConfig() {
    if [[ -f "${BUILD_CONFIG:=$DEFAULT_BUILD_CONFIG}" ]]; then
        source "$BUILD_CONFIG"
        load_build_config="true"
    fi
}

loadBuildConfig
initHostPlatforms

for i in "$@"; do
    case ${i,,} in
    -h | --help)
        printHelp
        exit 0
        ;;
    --disable-cgo)
        cgo_enabled="0"
        shift
        ;;
    --source-dir=*)
        source_dir="${i#*=}"
        shift
        ;;
    --more-go-cmd-args=*)
        addBuildArgs "${i#*=}"
        shift
        ;;
    --disable-micro)
        disable_micro="true"
        shift
        ;;
    --ldflags=*)
        addLDFLAGS "${i#*=}"
        shift
        ;;
    --platforms=*)
        platforms="${i#*=}"
        shift
        ;;
    --result-dir=*)
        result_dir="${i#*=}"
        shift
        ;;
    --tags=*)
        addTags "${i#*=}"
        shift
        ;;
    --show-all-targets)
        initPlatforms
        echo "${CURRENT_ALLOWED_PLATFORM}"
        exit 0
        ;;
    --github-proxy-mirror=*)
        gh_proxy="${i#*=}"
        shift
        ;;
    --force-gcc=*)
        force_cc="${i#*=}"
        shift
        ;;
    --force-g++=*)
        force_cxx="${i#*=}"
        shift
        ;;
    --host-cc=*)
        host_cc="${i#*=}"
        shift
        ;;
    --host-cxx=*)
        host_cxx="${i#*=}"
        shift
        ;;
    *)
        if declare -f parseDepArgs >/dev/null && parseDepArgs "$i"; then
            shift
            continue
        fi
        echo -e "${COLOR_RED}Invalid option: $i${COLOR_RESET}"
        exit 1
        ;;
    esac
done

fixArgs
initPlatforms
autoBuild "${platforms}"
