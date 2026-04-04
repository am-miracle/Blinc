//! Native file dialogs (open, save, folder picker)
//!
//! Cross-platform file dialog API using `rfd`. Available on desktop targets
//! when the `windowed` feature is enabled.
//!
//! # Example
//!
//! ```ignore
//! use blinc_app::dialog::{open_file, save_file, pick_folder, FileFilter};
//!
//! // Open a single file
//! if let Some(path) = open_file()
//!     .title("Open Image")
//!     .filter(FileFilter::new("Images").ext("png").ext("jpg").ext("gif"))
//!     .filter(FileFilter::new("All Files").ext("*"))
//!     .pick()
//! {
//!     println!("Selected: {}", path.display());
//! }
//!
//! // Open multiple files
//! let paths = open_file()
//!     .title("Select Files")
//!     .filter(FileFilter::new("Rust").ext("rs"))
//!     .pick_many();
//!
//! // Save dialog
//! if let Some(path) = save_file()
//!     .title("Save As")
//!     .file_name("untitled.txt")
//!     .filter(FileFilter::new("Text").ext("txt"))
//!     .save()
//! {
//!     println!("Save to: {}", path.display());
//! }
//!
//! // Folder picker
//! if let Some(dir) = pick_folder()
//!     .title("Choose Output Directory")
//!     .pick()
//! {
//!     println!("Directory: {}", dir.display());
//! }
//! ```

use std::path::PathBuf;

/// File type filter for dialogs.
///
/// ```ignore
/// FileFilter::new("Images").ext("png").ext("jpg").ext("webp")
/// ```
#[derive(Clone, Debug)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            extensions: Vec::new(),
        }
    }

    pub fn ext(mut self, extension: impl Into<String>) -> Self {
        self.extensions.push(extension.into());
        self
    }
}

// ============================================================================
// Open File Dialog
// ============================================================================

/// Builder for an open-file dialog.
pub struct OpenFileDialog {
    title: Option<String>,
    directory: Option<PathBuf>,
    filters: Vec<FileFilter>,
}

/// Start building an open-file dialog.
pub fn open_file() -> OpenFileDialog {
    OpenFileDialog {
        title: None,
        directory: None,
        filters: Vec::new(),
    }
}

impl OpenFileDialog {
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.directory = Some(dir.into());
        self
    }

    pub fn filter(mut self, filter: FileFilter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Show the dialog and return the selected file path, or `None` if cancelled.
    pub fn pick(self) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new();
        if let Some(title) = &self.title {
            d = d.set_title(title);
        }
        if let Some(dir) = &self.directory {
            d = d.set_directory(dir);
        }
        for f in &self.filters {
            let exts: Vec<&str> = f.extensions.iter().map(|s| s.as_str()).collect();
            d = d.add_filter(&f.name, &exts);
        }
        d.pick_file()
    }

    /// Show the dialog and return multiple selected file paths.
    pub fn pick_many(self) -> Vec<PathBuf> {
        let mut d = rfd::FileDialog::new();
        if let Some(title) = &self.title {
            d = d.set_title(title);
        }
        if let Some(dir) = &self.directory {
            d = d.set_directory(dir);
        }
        for f in &self.filters {
            let exts: Vec<&str> = f.extensions.iter().map(|s| s.as_str()).collect();
            d = d.add_filter(&f.name, &exts);
        }
        d.pick_files().unwrap_or_default()
    }
}

// ============================================================================
// Save File Dialog
// ============================================================================

/// Builder for a save-file dialog.
pub struct SaveFileDialog {
    title: Option<String>,
    directory: Option<PathBuf>,
    file_name: Option<String>,
    filters: Vec<FileFilter>,
}

/// Start building a save-file dialog.
pub fn save_file() -> SaveFileDialog {
    SaveFileDialog {
        title: None,
        directory: None,
        file_name: None,
        filters: Vec::new(),
    }
}

impl SaveFileDialog {
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.directory = Some(dir.into());
        self
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    pub fn filter(mut self, filter: FileFilter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Show the dialog and return the chosen save path, or `None` if cancelled.
    pub fn save(self) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new();
        if let Some(title) = &self.title {
            d = d.set_title(title);
        }
        if let Some(dir) = &self.directory {
            d = d.set_directory(dir);
        }
        if let Some(name) = &self.file_name {
            d = d.set_file_name(name);
        }
        for f in &self.filters {
            let exts: Vec<&str> = f.extensions.iter().map(|s| s.as_str()).collect();
            d = d.add_filter(&f.name, &exts);
        }
        d.save_file()
    }
}

// ============================================================================
// Folder Picker
// ============================================================================

/// Builder for a folder picker dialog.
pub struct FolderPickerDialog {
    title: Option<String>,
    directory: Option<PathBuf>,
}

/// Start building a folder picker dialog.
pub fn pick_folder() -> FolderPickerDialog {
    FolderPickerDialog {
        title: None,
        directory: None,
    }
}

impl FolderPickerDialog {
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.directory = Some(dir.into());
        self
    }

    /// Show the dialog and return the selected directory, or `None` if cancelled.
    pub fn pick(self) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new();
        if let Some(title) = &self.title {
            d = d.set_title(title);
        }
        if let Some(dir) = &self.directory {
            d = d.set_directory(dir);
        }
        d.pick_folder()
    }
}
