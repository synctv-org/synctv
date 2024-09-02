function parseDepArgs() {
    while [[ $# -gt 0 ]]; do
        case "${1}" in
        --version=*)
            VERSION="${1#*=}"
            shift
            ;;
        --skip-init-web)
            SKIP_INIT_WEB="true"
            shift
            ;;
        --web-version=*)
            WEB_VERSION="${1#*=}"
            shift
            ;;
        --web-repo=*)
            WEB_REPO="${1#*=}"
            shift
            ;;
        *)
            return 1
            ;;
        esac
    done
}

function printDepHelp() {
    echo -e "  ${COLOR_LIGHT_YELLOW}--version=<version>${COLOR_RESET}      - Set the build version (default: 'dev')."
    echo -e "  ${COLOR_LIGHT_YELLOW}--web-version=<version>${COLOR_RESET}   - Set the web dependency version (default: same as build version)."
    echo -e "  ${COLOR_LIGHT_YELLOW}--web-repo=<repo>${COLOR_RESET}        - Set the web repository (default: '<owner>/synctv-web')."
    echo -e "  ${COLOR_LIGHT_YELLOW}--skip-init-web${COLOR_RESET}           - Skip initializing the web dependency."
}

function printDepEnvHelp() {
    echo -e "  ${COLOR_LIGHT_GREEN}VERSION${COLOR_RESET}         - Set the build version (default: 'dev')."
    echo -e "  ${COLOR_LIGHT_GREEN}WEB_VERSION${COLOR_RESET}      - Set the web dependency version (default: same as build version)."
    echo -e "  ${COLOR_LIGHT_GREEN}WEB_REPO${COLOR_RESET}       - Set the web repository (default: '<owner>/synctv-web')."
    echo -e "  ${COLOR_LIGHT_GREEN}SKIP_INIT_WEB${COLOR_RESET}    - Skip initializing the web dependency (set to any non-empty value to enable)."
}

function initDepPlatforms() {
    clearAllowedPlatforms

    addAllowedPlatforms "linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64le,linux/riscv64,linux/s390x"
    addAllowedPlatforms "darwin/amd64,darwin/arm64"
    addAllowedPlatforms "windows/386,windows/amd64,windows/arm64"
    addAllowedPlatforms "freebsd/386,freebsd/amd64,freebsd/arm,freebsd/arm64"
    addAllowedPlatforms "netbsd/amd64"
    addAllowedPlatforms "openbsd/amd64,openbsd/arm64"
    addAllowedPlatforms "android/386,android/amd64,android/arm,android/arm64"

    addAllowedPlatforms "${GOHOSTOS}/${GOHOSTARCH}"
}

function initDep() {
    setDefault "VERSION" "dev"
    VERSION="$(echo "$VERSION" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
    echo -e "${COLOR_LIGHT_BLUE}Version:${COLOR_RESET} ${COLOR_LIGHT_CYAN}${VERSION}${COLOR_RESET}"
    if [[ "${VERSION}" != "dev" ]] && [[ ! "${VERSION}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-beta.*|-rc.*|-alpha.*)?$ ]]; then
        echo -e "${COLOR_LIGHT_RED}Version format error: ${VERSION}${COLOR_RESET}"
        return 1
    fi
    setDefault "WEB_VERSION" "${VERSION}"
    # 使用 git 命令获取仓库所有者，如果失败则使用默认值 "synctv-org"
    local repo_owner
    repo_owner=$(git config user.name 2>/dev/null || echo "synctv-org")
    setDefault "WEB_REPO" "${repo_owner}/synctv-web"
    setDefault "SKIP_INIT_WEB" ""

    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.Version=${VERSION}'"
    setDefault "WEB_VERSION" "${VERSION}"
    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.WebVersion=${WEB_VERSION}'"

    local git_commit
    git_commit="$(git log --pretty=format:"%h" -1)" || git_commit="unknown"
    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.GitCommit=${git_commit}'"

    if [[ -z "${SKIP_INIT_WEB}" ]] && [[ -n "${WEB_VERSION}" ]]; then
        echo -e "${COLOR_LIGHT_BLUE}Web repository:${COLOR_RESET} ${COLOR_LIGHT_CYAN}${WEB_REPO}${COLOR_RESET}"
        echo -e "${COLOR_LIGHT_BLUE}Web version:${COLOR_RESET} ${COLOR_LIGHT_CYAN}${WEB_VERSION}${COLOR_RESET}"
        downloadAndUnzip "https://github.com/${WEB_REPO}/releases/download/${WEB_VERSION}/dist.tar.gz" "${source_dir}/public/dist"
    fi

    addTags "jsoniter"
}
