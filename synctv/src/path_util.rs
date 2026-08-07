use std::path::Path;

pub fn absolute_display_path(path: &Path) -> String {
    if path.is_absolute() {
        return path.display().to_string();
    }

    std::env::current_dir()
        .map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
        .display()
        .to_string()
}
