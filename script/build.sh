#!/bin/bash
set -e

# Light Color definitions
readonly COLOR_LIGHT_RED='\033[1;31m'
readonly COLOR_LIGHT_GREEN='\033[1;32m'
readonly COLOR_LIGHT_YELLOW='\033[1;33m'
readonly COLOR_LIGHT_BLUE='\033[1;34m'
readonly COLOR_LIGHT_MAGENTA='\033[1;35m'
readonly COLOR_LIGHT_CYAN='\033[1;36m'
readonly COLOR_LIGHT_GRAY='\033[0;37m'
readonly COLOR_DARK_GRAY='\033[1;30m'
readonly COLOR_WHITE='\033[1;37m'
readonly COLOR_LIGHT_ORANGE='\033[1;91m'
readonly COLOR_RESET='\033[0m'

# Default values
readonly DEFAULT_SOURCE_DIR="$(pwd)"
readonly DEFAULT_RESULT_DIR="${DEFAULT_SOURCE_DIR}/build"
readonly DEFAULT_BUILD_CONFIG="${DEFAULT_SOURCE_DIR}/build.config.sh"
readonly DEFAULT_BUILD_MODE="default"
readonly DEFAULT_CGO_ENABLED="1"
readonly DEFAULT_FORCE_CGO="0"
readonly DEFAULT_CC="gcc"
readonly DEFAULT_CXX="g++"
readonly DEFAULT_CROSS_COMPILER_DIR="$(dirname $(mktemp -u))/cross"
readonly DEFAULT_CGO_FLAGS="-O2 -g0 -pipe"
readonly DEFAULT_CGO_LDFLAGS="-s"
readonly DEFAULT_LDFLAGS="-s -w -linkmode auto"
readonly DEFAULT_EXT_LDFLAGS=""
readonly DEFAULT_CGO_DEPS_VERSION="v0.5.1"
readonly DEFAULT_TTY_WIDTH="40"
readonly DEFAULT_NDK_VERSION="r27"

# Go environment variables
readonly GOHOSTOS="$(go env GOHOSTOS)"
readonly GOHOSTARCH="$(go env GOHOSTARCH)"
readonly GOHOSTPLATFORM="${GOHOSTOS}/${GOHOSTARCH}"
readonly GOVERSION="$(go env GOVERSION)" # e.g go1.23.1

# --- Function Declarations ---

# Prints help information about build configuration.
function printBuildConfigHelp() {
    echo -e "${COLOR_LIGHT_ORANGE}You can customize the build configuration using the following functions (defined in ${DEFAULT_BUILD_CONFIG}):${COLOR_RESET}"
    echo -e "  ${COLOR_LIGHT_GREEN}initDep${COLOR_RESET}          - Initialize dependencies"
    echo -e "  ${COLOR_LIGHT_GREEN}initDepPlatforms${COLOR_RESET} - Initialize dependency platforms"
    echo -e "  ${COLOR_LIGHT_GREEN}parseDepArgs${COLOR_RESET}     - Parse dependency arguments"
    echo -e "  ${COLOR_LIGHT_GREEN}printDepEnvHelp${COLOR_RESET}  - Print dependency environment variable help"
    echo -e "  ${COLOR_LIGHT_GREEN}printDepHelp${COLOR_RESET}     - Print dependency help information"
}

# Prints help information about environment variables.
function printEnvHelp() {
    echo -e "${COLOR_LIGHT_YELLOW}Environment Variables:${COLOR_RESET}"
    echo -e "  ${COLOR_LIGHT_CYAN}BIN_NAME${COLOR_RESET}           - Set the binary name (default: source directory basename)"
    echo -e "  ${COLOR_LIGHT_CYAN}BIN_NAME_NO_SUFFIX${COLOR_RESET} - Do not append the architecture suffix to the binary name"
    echo -e "  ${COLOR_LIGHT_CYAN}BUILD_CONFIG${COLOR_RESET}       - Set the build configuration file (default: ${DEFAULT_BUILD_CONFIG})"
    echo -e "  ${COLOR_LIGHT_CYAN}BUILD_MODE${COLOR_RESET}         - Set the build mode (default: ${DEFAULT_BUILD_MODE})"
    echo -e "  ${COLOR_LIGHT_CYAN}CGO_ENABLED${COLOR_RESET}        - Enable or disable CGO (default: ${DEFAULT_CGO_ENABLED})"
    echo -e "  ${COLOR_LIGHT_CYAN}CGO_FLAGS${COLOR_RESET}          - Set CGO flags (default: ${DEFAULT_CGO_FLAGS})"
    echo -e "  ${COLOR_LIGHT_CYAN}CGO_LDFLAGS${COLOR_RESET}        - Set CGO linker flags (default: ${DEFAULT_CGO_LDFLAGS})"
    echo -e "  ${COLOR_LIGHT_CYAN}CROSS_COMPILER_DIR${COLOR_RESET} - Set the cross compiler directory (default: ${DEFAULT_CROSS_COMPILER_DIR})"
    echo -e "  ${COLOR_LIGHT_CYAN}ENABLE_MICRO${COLOR_RESET}       - Enable building micro variants"
    echo -e "  ${COLOR_LIGHT_CYAN}FORCE_CC${COLOR_RESET}           - Force the use of a specific C compiler"
    echo -e "  ${COLOR_LIGHT_CYAN}FORCE_CGO${COLOR_RESET}          - Force the use of CGO (default: ${DEFAULT_FORCE_CGO})"
    echo -e "  ${COLOR_LIGHT_CYAN}FORCE_CXX${COLOR_RESET}          - Force the use of a specific C++ compiler"
    echo -e "  ${COLOR_LIGHT_CYAN}GH_PROXY${COLOR_RESET}           - Set the GitHub proxy mirror (e.g., https://mirror.ghproxy.com/)"
    echo -e "  ${COLOR_LIGHT_CYAN}HOST_CC${COLOR_RESET}            - Set the host C compiler (default: ${DEFAULT_CC})"
    echo -e "  ${COLOR_LIGHT_CYAN}HOST_CXX${COLOR_RESET}           - Set the host C++ compiler (default: ${DEFAULT_CXX})"
    echo -e "  ${COLOR_LIGHT_CYAN}NDK_VERSION${COLOR_RESET}        - Set the Android NDK version (default: ${DEFAULT_NDK_VERSION})"
    echo -e "  ${COLOR_LIGHT_CYAN}PLATFORM${COLOR_RESET}           - Set the target platform(s) (default: host platform, supports: all, linux, linux/arm*, ...)"
    echo -e "  ${COLOR_LIGHT_CYAN}RESULT_DIR${COLOR_RESET}         - Set the build result directory (default: ${DEFAULT_RESULT_DIR})"
    echo -e "  ${COLOR_LIGHT_CYAN}SOURCE_DIR${COLOR_RESET}         - Set the source directory (default: ${DEFAULT_SOURCE_DIR})"

    if declare -f printDepEnvHelp >/dev/null; then
        echo -e "${COLOR_LIGHT_GRAY}$(printSeparator)${COLOR_RESET}"
        echo -e "${COLOR_LIGHT_ORANGE}Dependency Environment Variables:${COLOR_RESET}"
        printDepEnvHelp
    fi
}

# Prints help information about command-line arguments.
function printHelp() {
    echo -e "${COLOR_LIGHT_GREEN}Usage:${COLOR_RESET}"
    echo -e "  $(basename "$0") [options]"
    echo -e ""
    echo -e "${COLOR_LIGHT_RED}Options:${COLOR_RESET}"
    echo -e "  ${COLOR_LIGHT_BLUE}--bin-name=<name>${COLOR_RESET}              - Specify the binary name (default: source directory basename)"
    echo -e "  ${COLOR_LIGHT_BLUE}--bin-name-no-suffix${COLOR_RESET}           - Do not append the architecture suffix to the binary name"
    echo -e "  ${COLOR_LIGHT_BLUE}--build-mode=<mode>${COLOR_RESET}            - Set the build mode (default: ${DEFAULT_BUILD_MODE})"
    echo -e "  ${COLOR_LIGHT_BLUE}--cross-compiler-dir=<dir>${COLOR_RESET}     - Specify the cross compiler directory (default: ${DEFAULT_CROSS_COMPILER_DIR})"
    echo -e "  ${COLOR_LIGHT_BLUE}--disable-cgo${COLOR_RESET}                  - Disable CGO support"
    echo -e "  ${COLOR_LIGHT_BLUE}-eh, --env-help${COLOR_RESET}                - Display help information about environment variables"
    echo -e "  ${COLOR_LIGHT_BLUE}--enable-micro${COLOR_RESET}                 - Enable building micro architecture variants"
    echo -e "  ${COLOR_LIGHT_BLUE}--ext-ldflags='<flags>'${COLOR_RESET}        - Set external linker flags (default: \"${DEFAULT_EXT_LDFLAGS}\")"
    echo -e "  ${COLOR_LIGHT_BLUE}--force-cgo${COLOR_RESET}                    - Force the use of CGO"
    echo -e "  ${COLOR_LIGHT_BLUE}--force-gcc=<path>${COLOR_RESET}             - Force the use of a specific C compiler"
    echo -e "  ${COLOR_LIGHT_BLUE}--force-g++=<path>${COLOR_RESET}             - Force the use of a specific C++ compiler"
    echo -e "  ${COLOR_LIGHT_BLUE}--github-proxy-mirror=<url>${COLOR_RESET}    - Use a GitHub proxy mirror (e.g., https://mirror.ghproxy.com/)"
    echo -e "  ${COLOR_LIGHT_BLUE}-h, --help${COLOR_RESET}                     - Display this help message"
    echo -e "  ${COLOR_LIGHT_BLUE}--host-gcc=<path>${COLOR_RESET}              - Specify the host C compiler (default: ${DEFAULT_CC})"
    echo -e "  ${COLOR_LIGHT_BLUE}--host-g++=<path>${COLOR_RESET}              - Specify the host C++ compiler (default: ${DEFAULT_CXX})"
    echo -e "  ${COLOR_LIGHT_BLUE}--ldflags='<flags>'${COLOR_RESET}            - Set linker flags (default: \"${DEFAULT_LDFLAGS}\")"
    echo -e "  ${COLOR_LIGHT_BLUE}--more-go-cmd-args='<args>'${COLOR_RESET}    - Pass additional arguments to the 'go build' command"
    echo -e "  ${COLOR_LIGHT_BLUE}--ndk-version=<version>${COLOR_RESET}        - Specify the Android NDK version (default: ${DEFAULT_NDK_VERSION})"
    echo -e "  ${COLOR_LIGHT_BLUE}-p=<platforms>, --platforms=<platforms>${COLOR_RESET} - Specify target platform(s) (default: host platform, supports: all, linux, linux/arm*, ...)"
    echo -e "  ${COLOR_LIGHT_BLUE}--result-dir=<dir>${COLOR_RESET}             - Specify the build result directory (default: ${DEFAULT_RESULT_DIR})"
    echo -e "  ${COLOR_LIGHT_BLUE}--show-all-platforms${COLOR_RESET}           - Display all supported target platforms"
    echo -e "  ${COLOR_LIGHT_BLUE}--source-dir=<dir>${COLOR_RESET}             - Specify the source directory (default: ${DEFAULT_SOURCE_DIR})"
    echo -e "  ${COLOR_LIGHT_BLUE}--tags='<tags>'${COLOR_RESET}                - Set build tags"

    if declare -f printDepHelp >/dev/null; then
        echo -e "${COLOR_LIGHT_MAGENTA}$(printSeparator)${COLOR_RESET}"
        echo -e "${COLOR_LIGHT_MAGENTA}Dependency Options:${COLOR_RESET}"
        printDepHelp
    fi

    echo -e "${COLOR_DARK_GRAY}$(printSeparator)${COLOR_RESET}"
    printBuildConfigHelp
}

# Sets a variable to a default value if it's not already set.
# Arguments:
#   $1: Variable name.
#   $2: Default value.
function setDefault() {
    local var_name="$1"
    local default_value="$2"
    [[ -z "${!var_name}" ]] && eval "${var_name}=\"${default_value}\"" || true
}

# Appends tags to the TAGS variable.
# Arguments:
#   $1: Tags to append.
function addTags() {
    TAGS="$(echo "$TAGS $@" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
}

# Appends linker flags to the LDFLAGS variable.
# Arguments:
#   $1: Linker flags to append.
function addLDFLAGS() {
    [[ -n "${1}" ]] && LDFLAGS="${LDFLAGS} ${1}" || true
}

function addExtLDFLAGS() {
    [[ -n "${1}" ]] && EXT_LDFLAGS="${EXT_LDFLAGS} ${1}" || true
}

# Appends build arguments to the BUILD_ARGS variable.
# Arguments:
#   $1: Build arguments to append.
function addBuildArgs() {
    [[ -n "${1}" ]] && BUILD_ARGS="${BUILD_ARGS} ${1}" || true
}

# Fixes and validates command-line arguments and sets default values.
function fixArgs() {
    setDefault "SOURCE_DIR" "${DEFAULT_SOURCE_DIR}"
    source_dir="$(cd "${SOURCE_DIR}" && pwd)"
    setDefault "BIN_NAME" "$(basename "${SOURCE_DIR}")"
    setDefault "BIN_NAME_NO_SUFFIX" ""
    setDefault "RESULT_DIR" "${DEFAULT_RESULT_DIR}"
    mkdir -p "${RESULT_DIR}"
    RESULT_DIR="$(cd "${RESULT_DIR}" && pwd)"
    echo -e "${COLOR_LIGHT_BLUE}Source directory: ${COLOR_LIGHT_GREEN}${source_dir}${COLOR_RESET}"
    echo -e "${COLOR_LIGHT_BLUE}Build result directory: ${COLOR_LIGHT_GREEN}${RESULT_DIR}${COLOR_RESET}"

    setDefault "CROSS_COMPILER_DIR" "$DEFAULT_CROSS_COMPILER_DIR"
    setDefault "PLATFORMS" "${GOHOSTPLATFORM}"
    setDefault "BUILD_MODE" "${DEFAULT_BUILD_MODE}"
    setDefault "ENABLE_MICRO" ""
    setDefault "FORCE_CGO" "${DEFAULT_FORCE_CGO}"
    setDefault "HOST_CC" "${DEFAULT_CC}"
    setDefault "HOST_CXX" "${DEFAULT_CXX}"
    setDefault "FORCE_CC" ""
    setDefault "FORCE_CXX" ""
    setDefault "GH_PROXY" ""
    setDefault "TAGS" ""
    setDefault "LDFLAGS" "${DEFAULT_LDFLAGS}"
    setDefault "EXT_LDFLAGS" "${DEFAULT_EXT_LDFLAGS}"
    setDefault "BUILD_ARGS" ""
    setDefault "CGO_DEPS_VERSION" "${DEFAULT_CGO_DEPS_VERSION}"
    setDefault "CGO_FLAGS" "${DEFAULT_CGO_FLAGS}"
    setDefault "CGO_LDFLAGS" "${DEFAULT_CGO_LDFLAGS}"
    setDefault "NDK_VERSION" "${DEFAULT_NDK_VERSION}"
}

# Checks if CGO is enabled.
# Returns:
#   0: CGO is enabled.
#   1: CGO is disabled.
function isCGOEnabled() {
    [[ "${CGO_ENABLED}" == "1" ]]
}

# Downloads a file from a URL and extracts it.
# Arguments:
#   $1: URL of the file to download.
#   $2: Directory to extract the file to.
#   $3: Optional. File type (e.g., "tgz", "zip"). If not provided, it's extracted from the URL.
function downloadAndUnzip() {
    local url="$1"
    local file="$2"
    local type="${3:-$(echo "${url}" | sed 's/.*\.//g')}"

    mkdir -p "${file}" || return $?
    file="$(cd "${file}" && pwd)" || return $?
    if [ "$(ls -A "${file}")" ]; then
        rm -rf "${file}"/* || return $?
    fi
    echo -e "${COLOR_LIGHT_BLUE}Downloading ${COLOR_LIGHT_CYAN}\"${url}\"${COLOR_LIGHT_BLUE} to ${COLOR_LIGHT_CYAN}\"${file}\"${COLOR_RESET}"

    local start_time=$(date +%s)

    case "${type}" in
    "tgz" | "gz")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -z || return $?
        ;;
    "bz2")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -j || return $?
        ;;
    "xz")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -J || return $?
        ;;
    "lzma")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 --lzma || return $?
        ;;
    "zip")
        curl -sL "${url}" -o "${file}/tmp.zip" || return $?
        unzip -q -o "${file}/tmp.zip" -d "${file}" || return $?
        rm -f "${file}/tmp.zip" || return $?
        ;;
    *)
        echo -e "${COLOR_LIGHT_RED}Unsupported compression type: ${type}${COLOR_RESET}"
        return 2
        ;;
    esac

    local end_time=$(date +%s)
    echo -e "${COLOR_LIGHT_GREEN}Download and extraction successful (took $((end_time - start_time))s)${COLOR_RESET}"
}

# --- Platform Management ---

# Removes duplicate platforms from a comma-separated list.
# Arguments:
#   $1: Comma-separated list of platforms.
# Returns:
#   Comma-separated list of platforms with duplicates removed.
function removeDuplicatePlatforms() {
    local all_platforms="$1"
    all_platforms="$(echo "${all_platforms}" | tr ',' '\n' | sort | uniq | paste -s -d ',' -)"
    all_platforms="${all_platforms#,}"
    all_platforms="${all_platforms%,}"
    echo "${all_platforms}"
}

# Adds platforms to the allowed platforms list.
# Arguments:
#   $1: Comma-separated list of platforms to add.
function addAllowedPlatforms() {
    [[ -z "$ALLOWED_PLATFORMS" ]] && ALLOWED_PLATFORMS="$1" || ALLOWED_PLATFORMS=$(removeDuplicatePlatforms "$ALLOWED_PLATFORMS,$1")
}

# Removes platforms from the allowed platforms list.
# Arguments:
#   $1: Comma-separated list of platforms to remove.
function deleteAllowedPlatforms() {
    ALLOWED_PLATFORMS=$(echo "${ALLOWED_PLATFORMS}" | sed "s|${1}$||g" | sed "s|${1},||g")
}

# Clears the allowed platforms list.
function clearAllowedPlatforms() {
    ALLOWED_PLATFORMS=""
}

# Initializes the platforms based on environment variables and allowed platforms.
# go tool dist list
function initPlatforms() {
    setDefault "CGO_ENABLED" "${DEFAULT_CGO_ENABLED}"

    local distList="$(go tool dist list)"
    addAllowedPlatforms "$(echo "$distList" | grep windows | grep -v arm)"
    addAllowedPlatforms "$(echo "$distList" | grep linux)"
    addAllowedPlatforms "$(echo "$distList" | grep android)"
    addAllowedPlatforms "$(echo "$distList" | grep darwin)"
    addAllowedPlatforms "$(echo "$distList" | grep ios)"

    addAllowedPlatforms "${GOHOSTOS}/${GOHOSTARCH}"

    if declare -f initDepPlatforms >/dev/null; then
        initDepPlatforms
    fi
}

# Checks if a platform is allowed.
# Arguments:
#   $1: Target platform to check.
#   $2: Optional. List of allowed platforms. If not provided, ALLOWED_PLATFORMS is used.
# Returns:
#   0: Platform is allowed.
#   1: Platform is not allowed.
function checkPlatform() {
    local target_platform="$1"

    if [[ "${ALLOWED_PLATFORMS}" =~ (^|,)${target_platform}($|,) ]]; then
        return 0
    else
        return 1
    fi
}

# Checks if a list of platforms are allowed.
# Arguments:
#   $1: Comma-separated list of platforms to check.
# Returns:
#   0: All platforms are allowed.
#   1: At least one platform is not allowed.
#   3: Error checking platforms.
function checkPlatforms() {
    for platform in ${1//,/ }; do
        case $(
            checkPlatform "${platform}"
            echo $?
        ) in
        0)
            continue
            ;;
        1)
            echo -e "${COLOR_LIGHT_RED}Platform not supported: ${platform}${COLOR_RESET}"
            return 1
            ;;
        *)
            echo -e "${COLOR_LIGHT_RED}Error checking platform: ${platform}${COLOR_RESET}"
            return 3
            ;;
        esac
    done
    return 0
}

# --- CGO Dependencies ---

function resetCGO() {
    CC=""
    CXX=""
    MORE_CGO_CFLAGS=""
    MORE_CGO_CXXFLAGS=""
    MORE_CGO_LDFLAGS=""

    EXTRA_PATH=""
}

# Initializes CGO dependencies based on the target operating system and architecture.
# Arguments:
#   $1: Target operating system (GOOS).
#   $2: Target architecture (GOARCH).
#   $3: Optional. Micro architecture variant.
# Returns:
#   0: CGO dependencies initialized successfully.
#   1: CGO disabled.
#   2: Error initializing CGO dependencies.
function initCGODeps() {
    resetCGO
    local goos="$1"
    local goarch="$2"
    local micro="$3"

    if [[ -n "${FORCE_CC}" ]] && [[ -n "${FORCE_CXX}" ]]; then
        CC="${FORCE_CC}"
        CXX="${FORCE_CXX}"
        return 0
    elif [[ -n "${FORCE_CC}" ]] || [[ -n "${FORCE_CXX}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Both FORCE_CC and FORCE_CXX must be set at the same time.${COLOR_RESET}"
        return 2
    fi

    initDefaultCGODeps "$@" || return $?

    PATH=${EXTRA_PATH:+$EXTRA_PATH:}$PATH absCCCXX || return $?

    return 0
}

function absCCCXX() {
    local cc_command cc_options
    read -r cc_command cc_options <<<"${CC}"
    CC="$(command -v "${cc_command}")" || return 2
    [[ -n "${cc_options}" ]] && CC="${CC} ${cc_options}"

    local cxx_command cxx_options
    read -r cxx_command cxx_options <<<"${CXX}"
    CXX="$(command -v "${cxx_command}")" || return 2
    [[ -n "${cxx_options}" ]] && CXX="${CXX} ${cxx_options}"

    return 0
}

# Initializes CGO dependencies for the host platform.
function initHostCGODeps() {
    CC="${HOST_CC}"
    CXX="${HOST_CXX}"
}

# Initializes default CGO dependencies based on the target operating system, architecture, and micro architecture.
# Arguments:
#   $1: Target operating system (GOOS).
#   $2: Target architecture (GOARCH).
#   $3: Optional. Micro architecture variant.
function initDefaultCGODeps() {
    local goos="$1"
    local goarch="$2"
    local micro="$3"
    local unamespacer="${GOHOSTOS}-${GOHOSTARCH}"
    [[ "${GOHOSTARCH}" == "arm" ]] && unamespacer="${GOHOSTOS}-arm32v7"

    case "${goos}" in
    "linux")
        case "${GOHOSTOS}" in
        "linux" | "darwin") ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
                return 0
            else
                echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
                return 1
            fi
            ;;
        esac

        case "${GOHOSTARCH}" in
        "amd64" | "arm64" | "arm" | "ppc64le" | "riscv64" | "s390x") ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
                return 0
            else
                echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
                return 1
            fi
            ;;
        esac

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
            initLinuxCGO "i686" "" || return $?
            ;;
        "amd64")
            initLinuxCGO "x86_64" "" || return $?
            ;;
        "arm")
            if [[ "${micro}" == "5" ]]; then
                initLinuxCGO "armv5" "eabi" || return $?
            else
                initLinuxCGO "armv${micro}" "eabihf" || return $?
            fi
            ;;
        "arm64")
            initLinuxCGO "aarch64" "" || return $?
            ;;
        "mips")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips" "" "${micro}" || return $?
            ;;
        "mipsle")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mipsel" "" "${micro}" || return $?
            ;;
        "mips64")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips64" "" "${micro}" || return $?
            ;;
        "mips64le")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips64el" "" "${micro}" || return $?
            ;;
        "ppc64")
            # initLinuxCGO "powerpc64" ""
            echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
            return 1
            ;;
        "ppc64le")
            initLinuxCGO "powerpc64le" "" || return $?
            ;;
        "riscv64")
            initLinuxCGO "riscv64" "" || return $?
            ;;
        "s390x")
            initLinuxCGO "s390x" "" || return $?
            ;;
        "loong64")
            initLinuxCGO "loongarch64" "" || return $?
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@" || return $?
            else
                echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
                return 1
            fi
            ;;
        esac
        ;;
    "windows")
        case "${GOHOSTOS}" in
        "linux" | "darwin") ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@" || return $?
                return 0
            else
                echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
                return 1
            fi
            ;;
        esac

        case "${GOHOSTARCH}" in
        "amd64" | "arm64" | "arm" | "ppc64le" | "riscv64" | "s390x") ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@" || return $?
                return 0
            else
                echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
                return 1
            fi
            ;;
        esac

        case "${goarch}" in
        "386")
            initWindowsCGO "i686" || return $?
            ;;
        "amd64")
            initWindowsCGO "x86_64" || return $?
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@" || return $?
            else
                echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
                return 1
            fi
            ;;
        esac
        ;;
    "android")
        case "${GOHOSTOS}" in
        "windows" | "linux")
            [[ "${GOHOSTARCH}" != "amd64" ]] && echo -e "${COLOR_LIGHT_RED}CGO is disabled for android/${goarch}${micro:+"/$micro"}.${COLOR_RESET}" && return 1
            ;;
        "darwin") ;;
        *)
            echo -e "${COLOR_LIGHT_RED}CGO is disabled for android/${goarch}${micro:+"/$micro"}.${COLOR_RESET}" && return 1
            ;;
        esac
        initAndroidNDK "${goarch}" "${micro}" || return $?
        ;;
    "darwin" | "ios")
        case "${GOHOSTOS}" in
        "darwin")
            CC="clang"
            CXX="clang++"
            ;;
        "linux")
            initOsxCGO "${goarch}" "${micro}" || return $?
            ;;
        *)
            echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
            return 1
            ;;
        esac
        ;;
    *)
        if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
            initHostCGODeps "$@" || return $?
        else
            echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}${micro:+"/$micro"}."
            return 1
        fi
        ;;
    esac
}

function initOsxCGO() {
    local goarch="$1"
    case "${GOHOSTOS}" in
    "darwin")
        CC="clang"
        CXX="clang++"
        ;;
    "linux")
        case "${GOHOSTARCH}" in
        "amd64")
            local cc_var=$(echo "CC_OSX_${goarch}" | awk '{print tolower($0)}')
            local cxx_var=$(echo "CXX_OSX_${goarch}" | awk '{print tolower($0)}')
            local cc=${!cc_var}
            local cxx=${!cxx_var}
            if [[ -z "${cc}" ]] && [[ -z "${cxx}" ]]; then
                if command -v oa64-clang >/dev/null 2>&1 && command -v oa64-clang++ >/dev/null 2>&1; then
                    cc="oa64-clang"
                    cxx="oa64-clang++"
                elif [[ -x "${CROSS_COMPILER_DIR}/osxcross/bin/oa64-clang" ]] && [[ -x "${CROSS_COMPILER_DIR}/osxcross/bin/oa64-clang++" ]]; then
                    cc="${CROSS_COMPILER_DIR}/osxcross/bin/oa64-clang"
                    cxx="${CROSS_COMPILER_DIR}/osxcross/bin/oa64-clang++"
                    EXTRA_PATH="${CROSS_COMPILER_DIR}/osxcross/bin"
                    patchelf --set-rpath "${CROSS_COMPILER_DIR}/osxcross/lib" \
                        ${CROSS_COMPILER_DIR}/osxcross/bin/x86_64-apple-darwin*-ld || return 2
                else
                    local ubuntu_version=$(lsb_release -rs 2>/dev/null || echo "18.04")
                    if [[ "$ubuntu_version" != *"."* ]]; then
                        ubuntu_version="18.04"
                    fi
                    downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/osxcross/releases/download/v0.1.1/osxcross-14-5-linux-amd64-gnu-ubuntu-${ubuntu_version}.tar.gz" \
                        "${CROSS_COMPILER_DIR}/osxcross" || return 2
                    cc="${CROSS_COMPILER_DIR}/osxcross/bin/oa64-clang"
                    cxx="${CROSS_COMPILER_DIR}/osxcross/bin/oa64-clang++"
                    EXTRA_PATH="${CROSS_COMPILER_DIR}/osxcross/bin"
                    patchelf --set-rpath "${CROSS_COMPILER_DIR}/osxcross/lib" \
                        ${CROSS_COMPILER_DIR}/osxcross/bin/x86_64-apple-darwin*-ld || return 2
                fi
            elif [[ -z "${cc}" ]] || [[ -z "${cxx}" ]]; then
                echo -e "${COLOR_LIGHT_RED}Both ${cc_var} and ${cxx_var} must be set.${COLOR_RESET}"
                return 2
            fi
            CC="${cc}"
            CXX="${cxx}"
            ;;
        *)
            echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}."
            return 1
            ;;
        esac
        ;;
    *)
        echo -e "${COLOR_LIGHT_YELLOW}CGO is disabled for ${goos}/${goarch}."
        return 1
        ;;
    esac
}

# Initializes CGO dependencies for Linux.
# Arguments:
#   $1: Architecture prefix (e.g., "i686", "x86_64").
#   $2: Optional. ABI (e.g., "eabi", "eabihf").
#   $3: Optional. Micro architecture variant.
function initLinuxCGO() {
    local arch_prefix="$1"
    local abi="$2"
    local micro="$3"
    local cc_var=$(echo "CC_LINUX_${arch_prefix}${abi}${micro}" | awk '{print tolower($0)}')
    local cxx_var=$(echo "CXX_LINUX_${arch_prefix}${abi}${micro}" | awk '{print tolower($0)}')
    local cc=${!cc_var}
    local cxx=${!cxx_var}

    if [[ -z "${cc}" ]] && [[ -z "${cxx}" ]]; then
        local cross_compiler_name="${arch_prefix}-linux-musl${abi}${micro}-cross"
        if command -v "${arch_prefix}-linux-musl${abi}${micro}-gcc" >/dev/null 2>&1 &&
            command -v "${arch_prefix}-linux-musl${abi}${micro}-g++" >/dev/null 2>&1; then
            cc="${arch_prefix}-linux-musl${abi}${micro}-gcc"
            cxx="${arch_prefix}-linux-musl${abi}${micro}-g++"
        elif [[ -x "${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc" ]] &&
            [[ -x "${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++" ]]; then
            cc="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc"
            cxx="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++"
        else
            downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${CGO_DEPS_VERSION}/${cross_compiler_name}-${unamespacer}.tgz" \
                "${CROSS_COMPILER_DIR}/${cross_compiler_name}" || return 2
            cc="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc"
            cxx="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++"
        fi
    elif [[ -z "${cc}" ]] || [[ -z "${cxx}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Both ${cc_var} and ${cxx_var} must be set.${COLOR_RESET}"
        return 2
    fi

    CC="${cc} -static --static"
    CXX="${cxx} -static --static"
}

# Initializes CGO dependencies for Windows.
# Arguments:
#   $1: Architecture prefix (e.g., "i686", "x86_64").
function initWindowsCGO() {
    local arch_prefix="$1"
    local cc_var=$(echo "CC_WINDOWS_${arch_prefix}" | awk '{print tolower($0)}')
    local cxx_var=$(echo "CXX_WINDOWS_${arch_prefix}" | awk '{print tolower($0)}')
    local cc=${!cc_var}
    local cxx=${!cxx_var}

    if [[ -z "${cc}" ]] && [[ -z "${cxx}" ]]; then
        local cross_compiler_name="${arch_prefix}-w64-mingw32-cross"
        if command -v "${arch_prefix}-w64-mingw32-gcc" >/dev/null 2>&1 &&
            command -v "${arch_prefix}-w64-mingw32-g++" >/dev/null 2>&1; then
            cc="${arch_prefix}-w64-mingw32-gcc"
            cxx="${arch_prefix}-w64-mingw32-g++"
        elif [[ -x "${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc" ]] &&
            [[ -x "${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++" ]]; then
            cc="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc"
            cxx="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++"
        else
            downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${CGO_DEPS_VERSION}/${cross_compiler_name}-${unamespacer}.tgz" \
                "${CROSS_COMPILER_DIR}/${cross_compiler_name}" || return 2
            cc="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc"
            cxx="${CROSS_COMPILER_DIR}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++"
        fi
    elif [[ -z "${cc}" ]] || [[ -z "${cxx}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Both ${cc_var} and ${cxx_var} must be set.${COLOR_RESET}"
        return 2
    fi

    CC="${cc} -static --static"
    CXX="${cxx} -static --static"
}

# Initializes CGO dependencies for Android NDK.
# Arguments:
#   $1: Target architecture (GOARCH).
function initAndroidNDK() {
    local goarch="$1"

    local ndk_dir="${CROSS_COMPILER_DIR}/android-ndk-${GOHOSTOS}-${NDK_VERSION}"
    local clang_base_dir="${ndk_dir}/toolchains/llvm/prebuilt/${GOHOSTOS}-x86_64/bin"
    local clang_prefix="$(getAndroidClang "${goarch}")"
    local cc="${clang_base_dir}/${clang_prefix}-clang"
    local cxx="${clang_base_dir}/${clang_prefix}-clang++"

    if [[ ! -d "${ndk_dir}" ]]; then
        local ndk_url="https://dl.google.com/android/repository/android-ndk-${NDK_VERSION}-${GOHOSTOS}.zip"
        downloadAndUnzip "${ndk_url}" "${ndk_dir}" "zip" || return 2
        mv "$ndk_dir/android-ndk-${NDK_VERSION}/"* "$ndk_dir"
        rmdir "$ndk_dir/android-ndk-${NDK_VERSION}" || return 2
    fi

    if [[ ! -x "${cc}" ]] || [[ ! -x "${cxx}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Android NDK not found or invalid. Please check the NDK_VERSION environment variable.${COLOR_RESET}"
        return 2
    fi

    CC="${cc}"
    CXX="${cxx}"
}

# Gets the Clang host prefix for Android NDK.
# Arguments:
#   $1: Target architecture (GOARCH).
# Returns:
#   The Clang host prefix.
function getAndroidClang() {
    local API="${API:-24}"
    case ${1} in
    arm)
        echo "armv7a-linux-androideabi${API}"
        ;;
    arm64)
        echo "aarch64-linux-android${API}"
        ;;
    386)
        echo "i686-linux-android${API}"
        ;;
    amd64)
        echo "x86_64-linux-android${API}"
        ;;
    esac
}

# --- Utility Functions ---

# Gets a separator line based on the terminal width.
# Returns:
#   A string of "-" characters with the length of the terminal width.
function printSeparator() {
    local width=$(tput cols 2>/dev/null || echo $DEFAULT_TTY_WIDTH)
    printf '%*s\n' "$width" '' | tr ' ' -
}

# --- Build Functions ---

# Builds a target for a specific platform and micro architecture variant.
# Arguments:
#   $1: Target platform (e.g., "linux/amd64").
#   $2: Target name (e.g., binary name).
# Ref:
# https://go.dev/wiki/MinimumRequirements#microarchitecture-support
# https://go.dev/doc/install/source#environment
function buildTarget() {
    local platform="$1"
    local goos="${platform%/*}"
    local goarch="${platform#*/}"

    echo -e "${COLOR_LIGHT_GRAY}$(printSeparator)${COLOR_RESET}"

    buildTargetWithMicro "${goos}" "${goarch}" ""

    if [ -z "${ENABLE_MICRO}" ]; then
        return 0
    fi

    # Build micro architecture variants based on the target architecture.
    case "${goarch}" in
    "386")
        echo
        buildTargetWithMicro "${goos}" "${goarch}" "sse2"
        echo
        buildTargetWithMicro "${goos}" "${goarch}" "softfloat"
        ;;
    "arm")
        for v in {5..7}; do
            echo
            buildTargetWithMicro "${goos}" "${goarch}" "$v"
        done
        ;;
    "arm64")
        if [[ ! "${GOVERSION#go}" > "1.23" ]]; then
            return 0
        fi
        for major in 8 9; do
            for minor in $(seq 0 $((major == 8 ? 9 : 5))); do
                echo
                buildTargetWithMicro "${goos}" "${goarch}" "v${major}.${minor}"
                echo
                buildTargetWithMicro "${goos}" "${goarch}" "v${major}.${minor},lse"
                echo
                buildTargetWithMicro "${goos}" "${goarch}" "v${major}.${minor},crypto"
            done
        done
        ;;
    "amd64")
        if [[ ! "${GOVERSION#go}" > "1.18" ]]; then
            return 0
        fi
        for v in {1..4}; do
            echo
            buildTargetWithMicro "${goos}" "${goarch}" "v$v"
        done
        ;;
    "mips" | "mipsle" | "mips64" | "mips64le")
        echo
        buildTargetWithMicro "${goos}" "${goarch}" "hardfloat"
        echo
        buildTargetWithMicro "${goos}" "${goarch}" "softfloat"
        ;;
    "ppc64" | "ppc64le")
        for version in 8 9 10; do
            echo
            buildTargetWithMicro "${goos}" "${goarch}" "power${version}"
        done
        ;;
    "wasm")
        echo
        buildTargetWithMicro "${goos}" "${goarch}" "satconv"
        echo
        buildTargetWithMicro "${goos}" "${goarch}" "signext"
        ;;
    esac
}

# Gets the extension based on the build mode and operating system.
# Arguments:
#   $1: Operating system (e.g., "linux", "windows", "darwin", "ios").
#   $2: Build mode (e.g., "archive", "shared", "default").
# Returns:
#   The extension string.
function extension() {
    local goos="$1"
    local buildmode="$2"
    if [ "$buildmode" == "archive" ] || [ "$buildmode" == "c-archive" ]; then
        if [ "$goos" == "windows" ]; then
            echo ".lib"
        else
            echo ".a"
        fi
    elif [ "$buildmode" == "shared" ] || [ "$buildmode" == "c-shared" ]; then
        if [ "$goos" == "windows" ]; then
            echo ".dll"
        elif [ "$goos" == "darwin" ] || [ "$goos" == "ios" ]; then
            echo ".dylib"
        else
            echo ".so"
        fi
    else
        if [ "$goos" == "windows" ]; then
            echo ".exe"
        fi
    fi
}

# Builds a target for a specific platform, micro architecture variant, and build environment.
# Arguments:
#   $1: GOOS
#   $2: GOARCH
#   $3: Micro architecture variant (e.g., "sse2", "softfloat"). Ref: https://go.dev/wiki/MinimumRequirements#microarchitecture-support
function buildTargetWithMicro() {
    local goos="$1"
    local goarch="$2"
    local micro="$3"
    local build_env=(
        "GOOS=${goos}"
        "GOARCH=${goarch}"
    )
    local buildmode=$BUILD_MODE
    local ext=$(extension "${goos}" "${buildmode}")
    local target_file="${RESULT_DIR}/${BIN_NAME}"
    [ -z "$BIN_NAME_NO_SUFFIX" ] && target_file="${target_file}-${goos}-${goarch}${micro:+"-${micro//[.,]/-}"}" || true
    target_file="${target_file}${ext}"

    # Set micro architecture specific environment variables.
    case "${goarch}" in
    "386")
        build_env+=("GO386=${micro}")
        [ -z "$micro" ] && micro="sse2"
        ;;
    "arm")
        build_env+=("GOARM=${micro}")
        [ -z "$micro" ] && micro="6"
        ;;
    "arm64")
        build_env+=("GOARM64=${micro}")
        [ -z "$micro" ] && micro="v8.0"
        ;;
    "amd64")
        build_env+=("GOAMD64=${micro}")
        ;;
    "mips" | "mipsle")
        build_env+=("GOMIPS=${micro}")
        [ -z "$micro" ] && micro="hardfloat"
        ;;
    "mips64" | "mips64le")
        build_env+=("GOMIPS64=${micro}")
        [ -z "$micro" ] && micro="hardfloat"
        ;;
    "ppc64" | "ppc64le")
        build_env+=("GOPPC64=${micro}")
        ;;
    "wasm")
        build_env+=("GOWASM=${micro}")
        ;;
    esac

    echo -e "${COLOR_LIGHT_MAGENTA}Building ${goos}/${goarch}${micro:+/${micro}}...${COLOR_RESET}"

    if isCGOEnabled; then
        if initCGODeps "${goos}" "${goarch}" "${micro}"; then
            code=0
        else
            code=$?
        fi

        build_env+=("PATH=${EXTRA_PATH:+$EXTRA_PATH:}$PATH")

        case "$code" in
        0)
            build_env+=("CGO_ENABLED=1")
            build_env+=("CC=${CC}")
            build_env+=("CXX=${CXX}")
            build_env+=("CGO_CFLAGS=${CGO_FLAGS}${MORE_CGO_CFLAGS:+ ${MORE_CGO_CFLAGS}}")
            build_env+=("CGO_CXXFLAGS=${CGO_FLAGS}${MORE_CGO_CXXFLAGS:+ ${MORE_CGO_CXXFLAGS}}")
            build_env+=("CGO_LDFLAGS=${CGO_LDFLAGS}${MORE_CGO_LDFLAGS:+ ${MORE_CGO_LDFLAGS}}")
            ;;
        1)
            if [[ "${FORCE_CGO}" == "1" ]]; then
                echo -e "${COLOR_LIGHT_RED}Error initializing CGO dependencies.${COLOR_RESET}"
                return 1
            fi
            build_env+=("CGO_ENABLED=0")
            ;;
        *)
            echo -e "${COLOR_LIGHT_RED}Error initializing CGO dependencies.${COLOR_RESET}"
            return 1
            ;;
        esac
    else
        if [[ "${FORCE_CGO}" == "1" ]]; then
            echo -e "${COLOR_LIGHT_RED}Error initializing CGO dependencies.${COLOR_RESET}"
            return 1
        fi
        build_env+=("CGO_ENABLED=0")
    fi

    echo -e "${COLOR_LIGHT_BLUE}Run command:\n${COLOR_WHITE}$(for var in "${build_env[@]}"; do
        key=$(echo "${var}" | cut -d= -f1)
        value=$(echo "${var}" | cut -d= -f2-)
        echo "export ${key}='${value}'"
    done)\n${COLOR_LIGHT_CYAN}go build -buildmode=$buildmode -trimpath ${BUILD_ARGS} -tags \"${TAGS}\" -ldflags \"${LDFLAGS}${EXT_LDFLAGS:+ -extldflags '$EXT_LDFLAGS'}\" -o \"${target_file}\" \"${source_dir}\"${COLOR_RESET}"
    local start_time=$(date +%s)
    env "${build_env[@]}" go build -buildmode=$buildmode -trimpath ${BUILD_ARGS} -tags "${TAGS}" -ldflags "${LDFLAGS}${EXT_LDFLAGS:+ -extldflags '$EXT_LDFLAGS'}" -o "${target_file}" "${source_dir}"
    local end_time=$(date +%s)
    echo -e "${COLOR_LIGHT_GREEN}Build successful: ${goos}/${goarch}${micro:+ ${micro}}  (took $((end_time - start_time))s)${COLOR_RESET}"
}

# Expands platform patterns (e.g., "linux/*") to a list of supported platforms.
# Arguments:
#   $1: Comma-separated list of platforms, potentially containing patterns.
# Returns:
#   Comma-separated list of expanded platforms.
function expandPlatforms() {
    local platforms="$1"
    local expanded_platforms=""
    for platform in ${platforms//,/ }; do
        if [[ "${platform}" == "all" ]]; then
            echo "${ALLOWED_PLATFORMS}"
            return 0
        elif [[ "${platform}" == *\** ]]; then
            for tmp_var in ${ALLOWED_PLATFORMS//,/ }; do
                [[ "${tmp_var}" == ${platform} ]] && expanded_platforms="${expanded_platforms},${tmp_var}"
            done
        elif [[ "${platform}" != */* ]]; then
            expanded_platforms="${expanded_platforms},$(expandPlatforms "${platform}/*")"
        else
            expanded_platforms="${expanded_platforms},${platform}"
        fi
    done
    removeDuplicatePlatforms "${expanded_platforms}"
}

# Performs the automatic build process for the specified platforms.
# Arguments:
#   $1: Comma-separated list of platforms to build for.
function autoBuild() {
    local platforms=$(expandPlatforms "$1")
    checkPlatforms "${platforms}" || return 1
    local start_time=$(date +%s)
    if declare -f initDep >/dev/null; then
        initDep
    fi
    local build_num=0
    for platform in ${platforms//,/ }; do
        buildTarget "${platform}"
        build_num=$((build_num + 1))
    done
    local end_time=$(date +%s)
    if [[ "${build_num}" -gt 1 ]]; then
        echo -e "${COLOR_LIGHT_YELLOW}Total took $((end_time - start_time))s${COLOR_RESET}"
    fi
}

# Checks if the build configuration file has been loaded.
# Returns:
#   0: Build configuration file has been loaded.
#   1: Build configuration file has not been loaded.
function loadedBuildConfig() {
    if [[ -n "${load_build_config}" ]]; then
        return 0
    fi
    return 1
}

# Loads the build configuration file if it exists.
function loadBuildConfig() {
    if [[ -f "${BUILD_CONFIG:=$DEFAULT_BUILD_CONFIG}" ]]; then
        source $BUILD_CONFIG
        load_build_config="true"
    fi
}

# --- Main Script ---

loadBuildConfig

# Parse command-line arguments.
while [[ $# -gt 0 ]]; do
    case "${1}" in
    -h | --help)
        printHelp
        exit 0
        ;;
    -eh | --env-help)
        printEnvHelp
        exit 0
        ;;
    --build-mode=*)
        BUILD_MODE="${1#*=}"
        ;;
    --disable-cgo)
        CGO_ENABLED="0"
        ;;
    --source-dir=*)
        SOURCE_DIR="${1#*=}"
        ;;
    --bin-name=*)
        BIN_NAME="${1#*=}"
        ;;
    --bin-name-no-suffix)
        BIN_NAME_NO_SUFFIX="true"
        ;;
    --more-go-cmd-args=*)
        addBuildArgs "${1#*=}"
        ;;
    --enable-micro)
        ENABLE_MICRO="true"
        ;;
    --ldflags=*)
        addLDFLAGS "${1#*=}"
        ;;
    --ext-ldflags=*)
        addExtLDFLAGS "${1#*=}"
        ;;
    -p=* | --platforms=*)
        PLATFORMS="${1#*=}"
        ;;
    --result-dir=*)
        RESULT_DIR="${1#*=}"
        ;;
    --tags=*)
        addTags "${1#*=}"
        ;;
    --show-all-platforms)
        initPlatforms
        echo "${ALLOWED_PLATFORMS}"
        exit 0
        ;;
    --show-all-platforms=*)
        initPlatforms
        echo "$(expandPlatforms "${1#*=}")"
        exit 0
        ;;
    --github-proxy-mirror=*)
        GH_PROXY="${1#*=}"
        ;;
    --cross-compiler-dir=*)
        CROSS_COMPILER_DIR="${1#*=}"
        ;;
    --force-gcc=*)
        FORCE_CC="${1#*=}"
        ;;
    --force-g++=*)
        FORCE_CXX="${1#*=}"
        ;;
    --host-gcc=*)
        HOST_CC="${1#*=}"
        ;;
    --host-g++=*)
        HOST_CXX="${1#*=}"
        ;;
    --ndk-version=*)
        NDK_VERSION="${1#*=}"
        ;;
    *)
        if declare -f parseDepArgs >/dev/null && parseDepArgs "$1"; then
            shift
            continue
        fi
        echo -e "${COLOR_LIGHT_RED}Invalid option: $1${COLOR_RESET}"
        exit 1
        ;;
    esac
    shift
done

fixArgs
initPlatforms
autoBuild "${PLATFORMS}"
