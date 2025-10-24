function parse_dep_args() {
    while [[ $# -gt 0 ]]; do
        case "${1}" in
        --version=*)
            VERSION="${1#*=}"
            shift
            ;;
        *)
            return 1
            ;;
        esac
    done
}

function print_dep_help() {
    echo -e "  ${COLOR_LIGHT_YELLOW}--version=<version>${COLOR_RESET}     - Set the build version (default: 'dev')"
}

function print_dep_env_help() {
    echo -e "  ${COLOR_LIGHT_GREEN}VERSION${COLOR_RESET}      - Set the build version (default: 'dev')"
}

function init_dep() {
    local git_commit
    git_commit="$(git rev-parse --short HEAD)" || git_commit="dev"
    echo -e "${COLOR_LIGHT_BLUE}Commit:${COLOR_RESET} ${COLOR_LIGHT_CYAN}${git_commit}${COLOR_RESET}"
    set_default "VERSION" "${git_commit}"

    # replace space, newline, and double quote
    VERSION="$(echo "$VERSION" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
    echo -e "${COLOR_LIGHT_BLUE}Version:${COLOR_RESET} ${COLOR_LIGHT_CYAN}${VERSION}${COLOR_RESET}"
    if [[ "${VERSION}" != "dev" ]] && [[ "${VERSION}" != "${git_commit}" ]] && [[ ! "${VERSION}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-beta.*|-rc.*|-alpha.*)?$ ]]; then
        echo -e "${COLOR_LIGHT_RED}Version format error: ${VERSION}${COLOR_RESET}"
        return 1
    fi

    add_ldflags "-X 'github.com/synctv-org/synctv/internal/version.Version=${VERSION}'"
    add_ldflags "-X 'github.com/synctv-org/synctv/internal/version.GitCommit=${git_commit}'"
    add_tags "jsoniter"
}
