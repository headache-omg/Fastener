#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("Fastener GUI is currently available on Windows only.");
}

#[cfg(windows)]
mod windows_app {
    use anyhow::{Context, Result, bail, ensure};
    use fastener::{
        CompressOptions, DIRECTORY_MAGIC, ProgressInfo, ProgressPhase,
        compress_directory_bundle_with_progress, compress_file_with_progress,
        decompress_directory_bundle_with_progress, decompress_file_with_progress,
        extract_zip_file_with_progress, verify_directory_bundle_with_progress,
        verify_file_with_progress, verify_zip_file_with_progress,
    };
    use fastener::{
        ENCRYPTED_MAGIC, compress_encrypted_with_progress, decompress_encrypted_with_progress,
        encrypted_is_directory, verify_encrypted_with_progress,
    };
    use rayon::ThreadPoolBuilder;
    use std::{
        ffi::{OsString, c_void},
        fs::File,
        io::Read,
        os::windows::ffi::OsStringExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicBool, AtomicU8, Ordering},
        thread,
        time::Instant,
    };
    use windows::{
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
            Globalization::GetUserDefaultUILanguage,
            Graphics::Gdi::{
                COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject, GetSysColorBrush, UpdateWindow,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                Controls::{
                    BST_CHECKED,
                    Dialogs::{
                        GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
                        OFN_PATHMUSTEXIST, OPENFILENAMEW,
                    },
                    EM_SETLIMITTEXT,
                },
                Input::KeyboardAndMouse::EnableWindow,
                Shell::{
                    BIF_EDITBOX, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, ILFree,
                    SHBrowseForFolderW, SHGetPathFromIDListW,
                },
                WindowsAndMessaging::*,
            },
        },
        core::{PCWSTR, PWSTR, w},
    };
    use zeroize::Zeroizing;

    const ID_PATH: i32 = 100;
    const ID_BROWSE: i32 = 101;
    const ID_COMPRESS: i32 = 105;
    const ID_DECOMPRESS: i32 = 106;
    const ID_VERIFY: i32 = 107;
    const ID_STATUS: i32 = 108;
    const ID_BROWSE_FOLDER: i32 = 109;
    const ID_MODE_FAST: i32 = 110;
    const ID_MODE_BALANCED: i32 = 111;
    const ID_MODE_DENSE: i32 = 112;
    const ID_LANG_JA: i32 = 113;
    const ID_LANG_EN: i32 = 114;
    const ID_INPUT_LABEL: i32 = 115;
    const ID_MODE_LABEL: i32 = 116;
    const ID_RESULT_LABEL: i32 = 117;
    const ID_ENCRYPT: i32 = 118;
    const ID_PASSWORD: i32 = 119;
    const ID_PASSWORD_CONFIRM: i32 = 120;
    const ID_PASSWORD_LABEL: i32 = 121;
    const ID_CONFIRM_LABEL: i32 = 122;
    const ID_PASSWORD_HELP: i32 = 123;
    const ID_RECOVERY_CREATE: i32 = 124;
    const ID_REPAIR: i32 = 125;
    const WM_FASTENER_PROGRESS: u32 = WM_APP + 1;
    const WM_FASTENER_FINISHED: u32 = WM_APP + 2;
    const WM_FASTENER_LANGUAGE: u32 = WM_APP + 3;
    static BUSY: AtomicBool = AtomicBool::new(false);
    static LANGUAGE: AtomicU8 = AtomicU8::new(Language::English as u8);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Language {
        Japanese = 0,
        English = 1,
    }

    impl Language {
        fn current() -> Self {
            match LANGUAGE.load(Ordering::Acquire) {
                0 => Self::Japanese,
                _ => Self::English,
            }
        }
    }

    fn text(language: Language, japanese: &'static str, english: &'static str) -> &'static str {
        match language {
            Language::Japanese => japanese,
            Language::English => english,
        }
    }

    macro_rules! localized_format {
        ($language:expr, $japanese:literal, $english:literal, $($argument:expr),+ $(,)?) => {
            match $language {
                Language::Japanese => format!($japanese, $($argument),+),
                Language::English => format!($english, $($argument),+),
            }
        };
    }

    struct UiMessage {
        text: String,
        error: bool,
    }

    pub fn run() -> Result<()> {
        let user_language = unsafe { GetUserDefaultUILanguage() };
        let language = if user_language & 0x03ff == 0x0011 {
            Language::Japanese
        } else {
            Language::English
        };
        LANGUAGE.store(language as u8, Ordering::Release);
        let logical_processors = thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        ThreadPoolBuilder::new()
            .num_threads(recommended_worker_threads(logical_processors))
            .build_global()
            .context(text(
                language,
                "CPU並列ワーカーを初期化できません",
                "Could not initialize CPU workers",
            ))?;
        unsafe {
            let module = GetModuleHandleW(None).context(text(
                language,
                "実行モジュールを取得できません",
                "Could not access the executable module",
            ))?;
            let instance = HINSTANCE(module.0);
            let class_name = w!("FastenerGuiWindow");
            let class = WNDCLASSW {
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hInstance: instance,
                lpszClassName: class_name,
                lpfnWndProc: Some(window_proc),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            ensure!(
                RegisterClassW(&class) != 0,
                text(
                    language,
                    "ウィンドウクラスを登録できません",
                    "Could not register the window class",
                )
            );

            let window_title = wide(text(
                language,
                "Fastener — 並列圧縮・解凍",
                "Fastener — Parallel compression and extraction",
            ));
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                PCWSTR(window_title.as_ptr()),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                760,
                680,
                None,
                None,
                Some(instance),
                None,
            )?;
            let _ = ShowWindow(window, SW_SHOW);
            ensure!(
                UpdateWindow(window).as_bool(),
                text(
                    language,
                    "ウィンドウを表示できません",
                    "Could not display the window",
                )
            );

            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    fn recommended_worker_threads(logical_processors: usize) -> usize {
        (logical_processors.saturating_mul(3) / 4).max(1)
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_CREATE => {
                unsafe { create_controls(window) };
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xffff) as i32;
                unsafe {
                    match id {
                        ID_BROWSE => browse(window),
                        ID_BROWSE_FOLDER => browse_folder(window),
                        ID_COMPRESS => start_operation(window, Operation::Compress),
                        ID_DECOMPRESS => start_operation(window, Operation::Decompress),
                        ID_VERIFY => start_operation(window, Operation::Verify),
                        ID_RECOVERY_CREATE => start_operation(window, Operation::RecoveryCreate),
                        ID_REPAIR => start_operation(window, Operation::Repair),
                        ID_LANG_JA => set_language(window, Language::Japanese),
                        ID_LANG_EN => set_language(window, Language::English),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_FASTENER_PROGRESS => {
                unsafe { receive_ui_message(window, lparam, false) };
                LRESULT(0)
            }
            WM_FASTENER_FINISHED => {
                BUSY.store(false, Ordering::Release);
                unsafe {
                    set_controls_enabled(window, true);
                    receive_ui_message(window, lparam, true);
                }
                LRESULT(0)
            }
            WM_FASTENER_LANGUAGE => {
                unsafe { apply_language(window, Language::current()) };
                LRESULT(0)
            }
            WM_CLOSE => {
                if BUSY.load(Ordering::Acquire) {
                    unsafe {
                        let message = wide(text(
                            Language::current(),
                            "大規模ファイルを処理中です。完了するまでお待ちください。",
                            "A large operation is running. Please wait for it to finish.",
                        ));
                        let _ = MessageBoxW(
                            Some(window),
                            PCWSTR(message.as_ptr()),
                            w!("Fastener"),
                            MB_OK | MB_ICONINFORMATION,
                        );
                    }
                } else {
                    unsafe {
                        let _ = DestroyWindow(window);
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }

    unsafe fn create_controls(parent: HWND) {
        unsafe {
            create(
                w!("STATIC"),
                w!("Fastener"),
                WS_CHILD | WS_VISIBLE,
                24,
                18,
                680,
                34,
                parent,
                0,
            );
            create(
                w!("BUTTON"),
                w!("日本語"),
                window_style(
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_GROUP,
                    BS_AUTORADIOBUTTON,
                ),
                545,
                18,
                80,
                28,
                parent,
                ID_LANG_JA,
            );
            create(
                w!("BUTTON"),
                w!("English"),
                window_style(WS_CHILD | WS_VISIBLE | WS_TABSTOP, BS_AUTORADIOBUTTON),
                630,
                18,
                90,
                28,
                parent,
                ID_LANG_EN,
            );
            create(
                w!("STATIC"),
                w!("圧縮・解凍するファイル、または圧縮するフォルダー"),
                WS_CHILD | WS_VISIBLE,
                24,
                62,
                680,
                22,
                parent,
                ID_INPUT_LABEL,
            );
            create(
                w!("EDIT"),
                w!(""),
                window_style(
                    WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP,
                    ES_AUTOHSCROLL,
                ),
                24,
                88,
                460,
                30,
                parent,
                ID_PATH,
            );
            create(
                w!("BUTTON"),
                w!("ファイル…"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                495,
                87,
                100,
                32,
                parent,
                ID_BROWSE,
            );
            create(
                w!("BUTTON"),
                w!("フォルダー…"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                605,
                87,
                120,
                32,
                parent,
                ID_BROWSE_FOLDER,
            );
            create(
                w!("STATIC"),
                w!("圧縮モード"),
                WS_CHILD | WS_VISIBLE,
                24,
                138,
                90,
                24,
                parent,
                ID_MODE_LABEL,
            );
            create(
                w!("BUTTON"),
                w!("最速（LZ4）"),
                window_style(
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_GROUP,
                    BS_AUTORADIOBUTTON,
                ),
                120,
                134,
                130,
                28,
                parent,
                ID_MODE_FAST,
            );
            create(
                w!("BUTTON"),
                w!("バランス（Zstd-1）"),
                window_style(WS_CHILD | WS_VISIBLE | WS_TABSTOP, BS_AUTORADIOBUTTON),
                255,
                134,
                170,
                28,
                parent,
                ID_MODE_BALANCED,
            );
            create(
                w!("BUTTON"),
                w!("高圧縮（Zstd-12）"),
                window_style(WS_CHILD | WS_VISIBLE | WS_TABSTOP, BS_AUTORADIOBUTTON),
                430,
                134,
                170,
                28,
                parent,
                ID_MODE_DENSE,
            );
            let _ = SendMessageW(
                GetDlgItem(Some(parent), ID_MODE_BALANCED).unwrap_or_default(),
                BM_SETCHECK,
                Some(WPARAM(BST_CHECKED.0 as usize)),
                Some(LPARAM(0)),
            );
            let initial_language = match Language::current() {
                Language::Japanese => ID_LANG_JA,
                Language::English => ID_LANG_EN,
            };
            let _ = SendMessageW(
                GetDlgItem(Some(parent), initial_language).unwrap_or_default(),
                BM_SETCHECK,
                Some(WPARAM(BST_CHECKED.0 as usize)),
                Some(LPARAM(0)),
            );

            create(
                w!("BUTTON"),
                w!("圧縮時に暗号化"),
                window_style(
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_GROUP,
                    BS_AUTOCHECKBOX,
                ),
                24,
                174,
                225,
                26,
                parent,
                ID_ENCRYPT,
            );
            create(
                w!("STATIC"),
                w!("暗号化書庫の解凍・検証にもパスワードが必要です"),
                WS_CHILD | WS_VISIBLE,
                260,
                178,
                465,
                24,
                parent,
                ID_PASSWORD_HELP,
            );
            create(
                w!("STATIC"),
                w!("パスワード"),
                WS_CHILD | WS_VISIBLE,
                24,
                208,
                90,
                24,
                parent,
                ID_PASSWORD_LABEL,
            );
            create(
                w!("STATIC"),
                w!("確認（圧縮時）"),
                WS_CHILD | WS_VISIBLE,
                390,
                208,
                105,
                24,
                parent,
                ID_CONFIRM_LABEL,
            );
            for (id, x, width) in [(ID_PASSWORD, 120, 255), (ID_PASSWORD_CONFIRM, 500, 225)] {
                create(
                    w!("EDIT"),
                    w!(""),
                    window_style(
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
                        ES_PASSWORD | ES_AUTOHSCROLL,
                    ),
                    x,
                    204,
                    width,
                    26,
                    parent,
                    id,
                );
                if let Ok(edit) = GetDlgItem(Some(parent), id) {
                    let _ =
                        SendMessageW(edit, EM_SETLIMITTEXT, Some(WPARAM(1024)), Some(LPARAM(0)));
                }
            }
            create(
                w!("BUTTON"),
                w!("圧縮"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                24,
                244,
                150,
                42,
                parent,
                ID_COMPRESS,
            );
            create(
                w!("BUTTON"),
                w!("解凍"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                186,
                244,
                150,
                42,
                parent,
                ID_DECOMPRESS,
            );
            create(
                w!("BUTTON"),
                w!("検証"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                348,
                244,
                150,
                42,
                parent,
                ID_VERIFY,
            );
            create(
                w!("STATIC"),
                w!("結果"),
                WS_CHILD | WS_VISIBLE,
                24,
                356,
                680,
                22,
                parent,
                ID_RESULT_LABEL,
            );
            create(
                w!("EDIT"),
                w!("Fastener 1.2.3"),
                window_style(
                    WS_CHILD | WS_VISIBLE | WS_BORDER | WS_VSCROLL,
                    ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY,
                ),
                24,
                382,
                701,
                236,
                parent,
                ID_STATUS,
            );

            let font = GetStockObject(DEFAULT_GUI_FONT);
            create(
                w!("BUTTON"),
                w!("復旧データ作成"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                24,
                300,
                240,
                38,
                parent,
                ID_RECOVERY_CREATE,
            );
            create(
                w!("BUTTON"),
                w!("修復（.parが必要）"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                280,
                300,
                240,
                38,
                parent,
                ID_REPAIR,
            );
            for id in [
                ID_RECOVERY_CREATE,
                ID_REPAIR,
                ID_PATH,
                ID_BROWSE,
                ID_BROWSE_FOLDER,
                ID_COMPRESS,
                ID_DECOMPRESS,
                ID_VERIFY,
                ID_STATUS,
                ID_MODE_FAST,
                ID_MODE_BALANCED,
                ID_MODE_DENSE,
                ID_LANG_JA,
                ID_LANG_EN,
                ID_INPUT_LABEL,
                ID_MODE_LABEL,
                ID_RESULT_LABEL,
                ID_ENCRYPT,
                ID_PASSWORD,
                ID_PASSWORD_CONFIRM,
                ID_PASSWORD_LABEL,
                ID_CONFIRM_LABEL,
                ID_PASSWORD_HELP,
            ] {
                if let Ok(control) = GetDlgItem(Some(parent), id) {
                    let _ = SendMessageW(
                        control,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            apply_language(parent, Language::current());
        }
    }

    // Mirrors the coordinate-oriented Win32 CreateWindowExW API.
    #[allow(clippy::too_many_arguments)]
    unsafe fn create(
        class: PCWSTR,
        text: PCWSTR,
        style: WINDOW_STYLE,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: HWND,
        id: i32,
    ) {
        let menu = if id == 0 {
            None
        } else {
            Some(HMENU(id as isize as *mut c_void))
        };
        let _ = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                text,
                style,
                x,
                y,
                width,
                height,
                Some(parent),
                menu,
                None,
                None,
            )
        };
    }

    fn window_style(base: WINDOW_STYLE, control_style: i32) -> WINDOW_STYLE {
        WINDOW_STYLE(base.0 | control_style as u32)
    }

    unsafe fn set_language(parent: HWND, language: Language) {
        LANGUAGE.store(language as u8, Ordering::Release);
        let _ = unsafe { PostMessageW(Some(parent), WM_FASTENER_LANGUAGE, WPARAM(0), LPARAM(0)) };
    }

    unsafe fn apply_language(parent: HWND, language: Language) {
        unsafe {
            set_text(
                parent,
                text(
                    language,
                    "Fastener — 並列圧縮・解凍",
                    "Fastener — Parallel compression and extraction",
                ),
            );
            for (id, japanese, english) in [
                (
                    ID_INPUT_LABEL,
                    "圧縮・解凍するファイル、または圧縮するフォルダー",
                    "File to compress/extract, or folder to compress",
                ),
                (ID_BROWSE, "ファイル…", "File…"),
                (ID_BROWSE_FOLDER, "フォルダー…", "Folder…"),
                (ID_MODE_LABEL, "圧縮モード", "Compression mode"),
                (ID_MODE_FAST, "最速（LZ4）", "Fast (LZ4)"),
                (ID_MODE_BALANCED, "バランス（Zstd-1）", "Balanced (Zstd-1)"),
                (ID_MODE_DENSE, "高圧縮（Zstd-12）", "Dense (Zstd-12)"),
                (ID_COMPRESS, "圧縮", "Compress"),
                (ID_DECOMPRESS, "解凍", "Extract"),
                (ID_VERIFY, "検証", "Verify"),
                (ID_RECOVERY_CREATE, "復旧データ作成", "Create recovery data"),
                (ID_REPAIR, "修復（.parが必要）", "Repair (requires .par)"),
                (ID_RESULT_LABEL, "結果", "Result"),
                (ID_ENCRYPT, "圧縮時に暗号化", "Encrypt on compression"),
                (ID_PASSWORD_LABEL, "パスワード", "Password"),
                (ID_CONFIRM_LABEL, "確認（圧縮時）", "Confirm (create)"),
                (
                    ID_PASSWORD_HELP,
                    "暗号化書庫の解凍・検証にもパスワードが必要です",
                    "Password required for encrypted archives.",
                ),
            ] {
                if let Ok(control) = GetDlgItem(Some(parent), id) {
                    set_text(control, text(language, japanese, english));
                }
            }
        }
    }

    unsafe fn browse(parent: HWND) {
        let language = Language::current();
        let mut path_buffer = [0u16; 32768];
        let filter: Vec<u16> = text(
            language,
            "すべての対応ファイル\0*.fst;*.*\0Fastener書庫 (*.fst)\0*.fst\0すべてのファイル (*.*)\0*.*\0\0",
            "All supported files\0*.fst;*.*\0Fastener archives (*.fst)\0*.fst\0All files (*.*)\0*.*\0\0",
        )
            .encode_utf16()
            .collect();
        let mut dialog = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: parent,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(path_buffer.as_mut_ptr()),
            nMaxFile: path_buffer.len() as u32,
            Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
            ..Default::default()
        };
        if unsafe { GetOpenFileNameW(&mut dialog).as_bool() } {
            let length = path_buffer.iter().position(|&c| c == 0).unwrap_or(0);
            let path = OsString::from_wide(&path_buffer[..length])
                .to_string_lossy()
                .into_owned();
            unsafe { set_text(GetDlgItem(Some(parent), ID_PATH).unwrap_or_default(), &path) };
            unsafe {
                set_status(
                    parent,
                    text(
                        language,
                        "ファイルを選択しました。操作を選んでください。",
                        "File selected. Choose an operation.",
                    ),
                )
            };
        }
    }

    unsafe fn browse_folder(parent: HWND) {
        let language = Language::current();
        let mut display_name = [0u16; 260];
        let title = wide(text(
            language,
            "1つのFSTへ圧縮するフォルダーを選択してください",
            "Select a folder to compress into one FST archive",
        ));
        let dialog = BROWSEINFOW {
            hwndOwner: parent,
            pszDisplayName: PWSTR(display_name.as_mut_ptr()),
            lpszTitle: PCWSTR(title.as_ptr()),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            ..Default::default()
        };
        let item = unsafe { SHBrowseForFolderW(&dialog) };
        if item.is_null() {
            return;
        }
        let mut path_buffer = [0u16; 260];
        let found = unsafe { SHGetPathFromIDListW(item, &mut path_buffer).as_bool() };
        unsafe { ILFree(Some(item)) };
        if found {
            let length = path_buffer.iter().position(|&c| c == 0).unwrap_or(0);
            let path = OsString::from_wide(&path_buffer[..length])
                .to_string_lossy()
                .into_owned();
            unsafe { set_text(GetDlgItem(Some(parent), ID_PATH).unwrap_or_default(), &path) };
            unsafe {
                set_status(
                    parent,
                    text(
                        language,
                        "フォルダーを選択しました。1つのFST書庫へ圧縮します。",
                        "Folder selected. It will be compressed into one FST archive.",
                    ),
                )
            };
        }
    }

    #[derive(Clone, Copy)]
    enum Operation {
        Compress,
        Decompress,
        Verify,
        RecoveryCreate,
        Repair,
    }

    unsafe fn start_operation(parent: HWND, operation: Operation) {
        if BUSY.load(Ordering::Acquire) {
            return;
        }
        let language = Language::current();
        let prepared = (|| -> Result<_> {
            let path_text = unsafe { get_text(GetDlgItem(Some(parent), ID_PATH)?)? };
            ensure!(
                !path_text.trim().is_empty(),
                text(
                    language,
                    "先にファイルまたはフォルダーを選択してください",
                    "Select a file or folder first",
                )
            );
            let input = PathBuf::from(path_text.trim());
            ensure!(
                input.exists(),
                text(
                    language,
                    "選択した入力が見つかりません",
                    "The selected input was not found",
                )
            );
            ensure!(
                input.is_file() || matches!(operation, Operation::Compress),
                text(
                    language,
                    "フォルダーに対応している操作は圧縮です",
                    "Folders can only be compressed",
                )
            );
            let compression_level = unsafe { selected_compression_level(parent) };
            let encrypt = unsafe {
                SendMessageW(
                    GetDlgItem(Some(parent), ID_ENCRYPT)?,
                    BM_GETCHECK,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                )
                .0 == BST_CHECKED.0 as isize
            };
            let needs_password = if matches!(operation, Operation::Compress) {
                encrypt
            } else if matches!(operation, Operation::RecoveryCreate) {
                false
            } else if matches!(operation, Operation::Repair) {
                fastener::recovery_info(&fastener::recovery_path(&input))?.encrypted
            } else {
                detect_archive_kind(&input, language)? == ArchiveKind::Encrypted
            };
            if matches!(operation, Operation::Compress) && !encrypt {
                let value = unsafe { get_secret(GetDlgItem(Some(parent), ID_PASSWORD)?)? };
                let confirmation =
                    unsafe { get_secret(GetDlgItem(Some(parent), ID_PASSWORD_CONFIRM)?)? };
                ensure!(
                    value.is_empty() && confirmation.is_empty(),
                    text(
                        language,
                        "パスワードを使う場合は「圧縮時に暗号化」にチェックしてください",
                        "Select Encrypt on compression to use a password"
                    )
                );
            }
            let password = if needs_password {
                let value = unsafe { get_secret(GetDlgItem(Some(parent), ID_PASSWORD)?)? };
                ensure!(
                    !value.is_empty() && value.len() <= 1024,
                    text(
                        language,
                        "パスワードを入力してください（UTF-8で1024バイト以内）",
                        "Enter a password (up to 1024 UTF-8 bytes)"
                    )
                );
                if matches!(operation, Operation::Compress) {
                    let confirm =
                        unsafe { get_secret(GetDlgItem(Some(parent), ID_PASSWORD_CONFIRM)?)? };
                    ensure!(
                        *value == *confirm,
                        text(
                            language,
                            "確認用パスワードが一致しません",
                            "Passwords do not match"
                        )
                    );
                }
                Some(value)
            } else {
                None
            };
            match operation {
                Operation::RecoveryCreate => {
                    unsafe {
                        confirm_overwrite(parent, &fastener::recovery_path(&input), language)?
                    };
                }
                Operation::Repair => {
                    unsafe {
                        confirm_overwrite(parent, &fastener::repaired_path(&input), language)?
                    };
                }
                Operation::Compress if input.is_file() => {
                    unsafe { confirm_overwrite(parent, &compressed_path(&input), language)? };
                }
                Operation::Decompress => {
                    if matches!(
                        detect_archive_kind(&input, language)?,
                        ArchiveKind::Fastener
                    ) {
                        unsafe { confirm_overwrite(parent, &restored_path(&input), language)? };
                    }
                }
                _ => {}
            }
            Ok((input, compression_level, password))
        })();

        let (input, compression_level, password) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                unsafe {
                    show_error(
                        parent,
                        &format!("{}\r\n\r\n{error:#}", text(language, "エラー", "Error")),
                    )
                };
                return;
            }
        };
        if BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        unsafe {
            for id in [ID_PASSWORD, ID_PASSWORD_CONFIRM] {
                if let Ok(edit) = GetDlgItem(Some(parent), id) {
                    set_text(edit, "");
                }
            }
            set_controls_enabled(parent, false);
            set_status(
                parent,
                text(
                    language,
                    "バックグラウンド処理を開始しています…",
                    "Starting the background operation…",
                ),
            );
        }
        let parent_value = parent.0 as usize;
        thread::spawn(move || {
            let parent = HWND(parent_value as *mut c_void);
            let result = perform_operation(
                parent,
                operation,
                input,
                compression_level,
                password,
                language,
            );
            let message = match result {
                Ok(text) => UiMessage { text, error: false },
                Err(error) => UiMessage {
                    text: format!("{}\r\n\r\n{error:#}", text(language, "エラー", "Error")),
                    error: true,
                },
            };
            post_ui_message(parent, WM_FASTENER_FINISHED, message);
        });
    }

    fn perform_operation(
        parent: HWND,
        operation: Operation,
        input: PathBuf,
        compression_level: i32,
        password: Option<Zeroizing<String>>,
        language: Language,
    ) -> Result<String> {
        let started = Instant::now();
        if matches!(operation, Operation::RecoveryCreate | Operation::Repair) {
            let output = if matches!(operation, Operation::RecoveryCreate) {
                let output = fastener::recovery_path(&input);
                let plan = fastener::recovery_plan(std::fs::metadata(&input)?.len())?;
                let detail = format!(
                    "{}: {} bytes ({:.2}%)",
                    text(language, "追加容量", "Additional storage"),
                    plan.recovery_bytes,
                    plan.recovery_bytes as f64 * 100.0 / plan.original_bytes.max(1) as f64
                );
                fastener::create_recovery_with_progress(&input, &output, |info| {
                    show_progress(parent, &detail, info, started, language)
                })?;
                output
            } else {
                let output = fastener::repaired_path(&input);
                let detail = input.display().to_string();
                fastener::repair_with_progress(
                    &input,
                    &fastener::recovery_path(&input),
                    &output,
                    password.as_ref().map(|p| p.as_bytes()),
                    |info| show_progress(parent, &detail, info, started, language),
                )?;
                output
            };
            return Ok(format!(
                "{}\r\n{}",
                text(language, "完了しました", "Completed"),
                output.display()
            ));
        }
        if let Some(password) = password {
            return perform_encrypted_operation(
                parent,
                operation,
                &input,
                compression_level,
                &password,
                started,
                language,
            );
        }
        match operation {
            Operation::RecoveryCreate | Operation::Repair => unreachable!(),
            Operation::Compress => {
                if input.is_dir() {
                    compress_directory(parent, &input, compression_level, started, language)
                } else {
                    let output = compressed_path(&input);
                    let detail = input.display().to_string();
                    let stats = compress_file_with_progress(
                        &input,
                        &output,
                        &CompressOptions {
                            compression_level,
                            ..Default::default()
                        },
                        |info| show_progress(parent, &detail, info, started, language),
                    )?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "圧縮が完了しました。\r\n\r\n入力: {}\r\n出力: {}\r\nモード: {}\r\nサイズ: {} → {} ({:.1}%)\r\n圧縮性: {}\r\nチャンク: {}\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "Compression completed.\r\n\r\nInput: {}\r\nOutput: {}\r\nMode: {}\r\nSize: {} -> {} ({:.1}%)\r\nCompressibility: {}\r\nChunks: {}\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        output.display(),
                        compression_mode_label(compression_level, language),
                        human_bytes(stats.original_size),
                        human_bytes(stats.archive_size),
                        stats.ratio() * 100.0,
                        compression_assessment(stats.ratio(), language),
                        stats.chunk_count,
                        elapsed.as_secs_f64(),
                        rate_labels(stats.original_size, elapsed, language)
                    ))
                }
            }
            Operation::Decompress => match detect_archive_kind(&input, language)? {
                ArchiveKind::Fastener => {
                    let output = restored_path(&input);
                    let detail = input.display().to_string();
                    let report = decompress_file_with_progress(&input, &output, |info| {
                        show_progress(parent, &detail, info, started, language)
                    })?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "Fastener書庫の並列解凍とチェックサム検証が完了しました。\r\n\r\n入力: {}\r\n出力: {}\r\n復元サイズ: {}\r\nチャンク: {}\r\nCPUワーカー: 実使用 {} / 上限 {} スレッド\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "Fastener extraction and checksum verification completed.\r\n\r\nInput: {}\r\nOutput: {}\r\nRestored size: {}\r\nChunks: {}\r\nCPU workers: {} used / {} available\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        output.display(),
                        human_bytes(report.original_size),
                        report.chunk_count,
                        report.worker_threads,
                        rayon::current_num_threads(),
                        elapsed.as_secs_f64(),
                        rate_labels(report.original_size, elapsed, language)
                    ))
                }
                ArchiveKind::FastenerDirectory => {
                    let output = directory_bundle_output(&input);
                    let detail = input.display().to_string();
                    let report =
                        decompress_directory_bundle_with_progress(&input, &output, |info| {
                            show_progress(parent, &detail, info, started, language)
                        })?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "Fastenerフォルダー書庫の展開が完了しました。\r\n\r\n入力: {}\r\n出力フォルダー: {}\r\nファイル数: {}\r\n展開サイズ: {}\r\nCPUワーカー: 実使用 {} / 上限 {} スレッド\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "Fastener folder archive extraction completed.\r\n\r\nInput: {}\r\nOutput folder: {}\r\nFiles: {}\r\nExtracted size: {}\r\nCPU workers: {} used / {} available\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        output.display(),
                        report.files,
                        human_bytes(report.original_size),
                        report.worker_threads,
                        rayon::current_num_threads(),
                        elapsed.as_secs_f64(),
                        rate_labels(report.original_size, elapsed, language)
                    ))
                }
                ArchiveKind::Zip => {
                    let output = zip_output_directory(&input);
                    let detail = input.display().to_string();
                    let report = extract_zip_file_with_progress(&input, &output, |info| {
                        show_progress(parent, &detail, info, started, language)
                    })?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "ZIPの並列展開が完了しました。\r\n\r\n入力: {}\r\n出力フォルダー: {}\r\nファイル数: {}\r\n展開サイズ: {}\r\nCPUワーカー: 実使用 {} / 上限 {} スレッド（エントリー単位）\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "Parallel ZIP extraction completed.\r\n\r\nInput: {}\r\nOutput folder: {}\r\nFiles: {}\r\nExtracted size: {}\r\nCPU workers: {} used / {} available (per entry)\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        output.display(),
                        report.entries,
                        human_bytes(report.uncompressed_size),
                        report.worker_threads,
                        rayon::current_num_threads(),
                        elapsed.as_secs_f64(),
                        rate_labels(report.uncompressed_size, elapsed, language)
                    ))
                }
                ArchiveKind::Unknown | ArchiveKind::Encrypted => bail!(
                    "{}",
                    text(
                        language,
                        "対応していない形式です。解凍できるのはFastener .fstまたは通常のZIPです。",
                        "Unsupported format. Fastener .fst and conventional ZIP archives can be extracted.",
                    )
                ),
            },
            Operation::Verify => match detect_archive_kind(&input, language)? {
                ArchiveKind::Fastener => {
                    let detail = input.display().to_string();
                    let report = verify_file_with_progress(&input, |info| {
                        show_progress(parent, &detail, info, started, language)
                    })?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "Fastener検証OK — 破損は検出されませんでした。\r\n\r\n書庫: {}\r\n元サイズ: {}\r\n書庫サイズ: {}\r\nチャンク: {}\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "Fastener verification passed — no corruption detected.\r\n\r\nArchive: {}\r\nOriginal size: {}\r\nArchive size: {}\r\nChunks: {}\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        human_bytes(report.original_size),
                        human_bytes(report.archive_size),
                        report.chunk_count,
                        elapsed.as_secs_f64(),
                        rate_labels(report.original_size, elapsed, language)
                    ))
                }
                ArchiveKind::FastenerDirectory => {
                    let detail = input.display().to_string();
                    let report = verify_directory_bundle_with_progress(&input, |info| {
                        show_progress(parent, &detail, info, started, language)
                    })?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "Fastenerフォルダー書庫の検証OK。\r\n\r\n書庫: {}\r\nファイル数: {}\r\n元サイズ: {}\r\n書庫サイズ: {}\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "Fastener folder archive verification passed.\r\n\r\nArchive: {}\r\nFiles: {}\r\nOriginal size: {}\r\nArchive size: {}\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        report.files,
                        human_bytes(report.original_size),
                        human_bytes(report.archive_size),
                        elapsed.as_secs_f64(),
                        rate_labels(report.original_size, elapsed, language)
                    ))
                }
                ArchiveKind::Zip => {
                    let detail = input.display().to_string();
                    let report = verify_zip_file_with_progress(&input, |info| {
                        show_progress(parent, &detail, info, started, language)
                    })?;
                    let elapsed = started.elapsed();
                    Ok(localized_format!(
                        language,
                        "ZIP並列検証OK — 全エントリーを読み取れました。\r\n\r\n書庫: {}\r\nファイル数: {}\r\n展開サイズ: {}\r\n時間: {:.3} 秒\r\n平均速度: {}",
                        "ZIP verification passed — every entry was read.\r\n\r\nArchive: {}\r\nFiles: {}\r\nExpanded size: {}\r\nTime: {:.3} seconds\r\nAverage speed: {}",
                        input.display(),
                        report.entries,
                        human_bytes(report.uncompressed_size),
                        elapsed.as_secs_f64(),
                        rate_labels(report.uncompressed_size, elapsed, language)
                    ))
                }
                ArchiveKind::Unknown | ArchiveKind::Encrypted => bail!(
                    "{}",
                    text(
                        language,
                        "対応していない形式です。検証できるのはFastener .fstまたは通常のZIPです。",
                        "Unsupported format. Fastener .fst and conventional ZIP archives can be verified.",
                    )
                ),
            },
        }
    }

    fn perform_encrypted_operation(
        parent: HWND,
        operation: Operation,
        input: &Path,
        level: i32,
        password: &str,
        started: Instant,
        language: Language,
    ) -> Result<String> {
        let detail = input.display().to_string();
        let progress = |info| show_progress(parent, &detail, info, started, language);
        let (report, output, label) = match operation {
            Operation::RecoveryCreate | Operation::Repair => unreachable!(),
            Operation::Compress => {
                let output = if input.is_dir() {
                    directory_archive_path(input)
                } else {
                    compressed_path(input)
                };
                let options = CompressOptions {
                    compression_level: level,
                    ..Default::default()
                };
                let report = compress_encrypted_with_progress(
                    input,
                    &output,
                    &options,
                    password.as_bytes(),
                    progress,
                )?;
                (
                    report,
                    Some(output),
                    text(
                        language,
                        "暗号化と圧縮が完了しました",
                        "Encryption and compression completed",
                    ),
                )
            }
            Operation::Decompress => {
                let output = if encrypted_is_directory(input)? {
                    directory_bundle_output(input)
                } else {
                    restored_path(input)
                };
                let report = decompress_encrypted_with_progress(
                    input,
                    &output,
                    password.as_bytes(),
                    progress,
                )?;
                (
                    report,
                    Some(output),
                    text(
                        language,
                        "復号・解凍・認証が完了しました",
                        "Decryption, extraction and authentication completed",
                    ),
                )
            }
            Operation::Verify => {
                let report = verify_encrypted_with_progress(input, password.as_bytes(), progress)?;
                (
                    report,
                    None,
                    text(
                        language,
                        "暗号化書庫の認証・チェックサム検証OK",
                        "Encrypted archive authentication and checksums passed",
                    ),
                )
            }
        };
        Ok(localized_format!(
            language,
            "{}\r\n\r\nファイル数: {}\r\n元サイズ: {}\r\n書庫サイズ: {}\r\nチャンク: {}\r\n時間: {:.3} 秒\r\n出力: {}",
            "{}\r\n\r\nFiles: {}\r\nOriginal: {}\r\nArchive: {}\r\nChunks: {}\r\nTime: {:.3} seconds\r\nOutput: {}",
            label,
            report.files,
            human_bytes(report.original_size),
            human_bytes(report.archive_size),
            report.chunks,
            started.elapsed().as_secs_f64(),
            output
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "—".to_owned())
        ))
    }

    fn compress_directory(
        parent: HWND,
        input: &Path,
        compression_level: i32,
        started: Instant,
        language: Language,
    ) -> Result<String> {
        let output = directory_archive_path(input);
        let detail = input.display().to_string();
        let report = compress_directory_bundle_with_progress(
            input,
            &output,
            &CompressOptions {
                compression_level,
                ..Default::default()
            },
            |info| show_progress(parent, &detail, info, started, language),
        )?;
        let elapsed = started.elapsed();
        let ratio = if report.original_size == 0 {
            1.0
        } else {
            report.archive_size as f64 / report.original_size as f64
        };
        Ok(localized_format!(
            language,
            "フォルダーを1つのFSTへ圧縮しました。\r\n\r\n入力フォルダー: {}\r\n出力書庫: {}\r\nモード: {}\r\nファイル数: {}\r\nサイズ: {} → {} ({:.1}%)\r\n圧縮性: {}\r\nCPU並列度: {} スレッド\r\n時間: {:.3} 秒\r\n平均速度: {}",
            "Folder compressed into one FST archive.\r\n\r\nInput folder: {}\r\nOutput archive: {}\r\nMode: {}\r\nFiles: {}\r\nSize: {} -> {} ({:.1}%)\r\nCompressibility: {}\r\nCPU parallelism: {} threads\r\nTime: {:.3} seconds\r\nAverage speed: {}",
            input.display(),
            output.display(),
            compression_mode_label(compression_level, language),
            report.files,
            human_bytes(report.original_size),
            human_bytes(report.archive_size),
            ratio * 100.0,
            compression_assessment(ratio, language),
            rayon::current_num_threads(),
            elapsed.as_secs_f64(),
            rate_labels(report.original_size, elapsed, language)
        ))
    }

    fn directory_archive_path(input: &Path) -> PathBuf {
        let parent = input.parent().unwrap_or_else(|| Path::new("."));
        let name = input
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "fastener-output".to_owned());
        for suffix in 1.. {
            let label = if suffix == 1 {
                format!("{name}.fst")
            } else {
                format!("{name}-{suffix}.fst")
            };
            let candidate = parent.join(label);
            if !candidate.exists() {
                return candidate;
            }
        }
        unreachable!()
    }

    fn directory_bundle_output(input: &Path) -> PathBuf {
        let base = input.with_extension("");
        unique_output_path(&base, false)
    }

    fn show_progress(
        parent: HWND,
        detail: &str,
        info: ProgressInfo,
        started: Instant,
        language: Language,
    ) {
        let percent = if info.total == 0 {
            100.0
        } else {
            info.completed as f64 / info.total as f64 * 100.0
        };
        let message = localized_format!(
            language,
            "処理中: {}\r\n{}\r\n\r\n進捗: {:.1}%  ({} / {})\r\n経過: {:.1} 秒\r\n実測速度: {}\r\nCPU並列度: {} スレッド",
            "Processing: {}\r\n{}\r\n\r\nProgress: {:.1}%  ({} / {})\r\nElapsed: {:.1} seconds\r\nMeasured speed: {}\r\nCPU parallelism: {} threads",
            phase_label(info.phase, language),
            detail,
            percent,
            human_bytes(info.completed),
            human_bytes(info.total),
            started.elapsed().as_secs_f64(),
            rate_labels(info.completed, started.elapsed(), language),
            rayon::current_num_threads()
        );
        post_ui_message(
            parent,
            WM_FASTENER_PROGRESS,
            UiMessage {
                text: message,
                error: false,
            },
        );
    }

    fn post_ui_message(parent: HWND, message: u32, payload: UiMessage) {
        let pointer = Box::into_raw(Box::new(payload));
        let posted =
            unsafe { PostMessageW(Some(parent), message, WPARAM(0), LPARAM(pointer as isize)) };
        if posted.is_err() {
            unsafe {
                drop(Box::from_raw(pointer));
            }
        }
    }

    unsafe fn receive_ui_message(parent: HWND, lparam: LPARAM, finished: bool) {
        let pointer = lparam.0 as *mut UiMessage;
        if pointer.is_null() {
            return;
        }
        let payload = unsafe { Box::from_raw(pointer) };
        unsafe { set_status(parent, &payload.text) };
        if finished && payload.error {
            unsafe { show_error(parent, &payload.text) };
        }
    }

    unsafe fn show_error(parent: HWND, message: &str) {
        unsafe { set_status(parent, message) };
        let wide = wide(message);
        unsafe {
            let _ = MessageBoxW(
                Some(parent),
                PCWSTR(wide.as_ptr()),
                w!("Fastener"),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    unsafe fn set_controls_enabled(parent: HWND, enabled: bool) {
        for id in [
            ID_RECOVERY_CREATE,
            ID_REPAIR,
            ID_PATH,
            ID_BROWSE,
            ID_BROWSE_FOLDER,
            ID_COMPRESS,
            ID_DECOMPRESS,
            ID_VERIFY,
            ID_MODE_FAST,
            ID_MODE_BALANCED,
            ID_MODE_DENSE,
            ID_LANG_JA,
            ID_LANG_EN,
            ID_ENCRYPT,
            ID_PASSWORD,
            ID_PASSWORD_CONFIRM,
        ] {
            if let Ok(control) = unsafe { GetDlgItem(Some(parent), id) } {
                let _ = unsafe { EnableWindow(control, enabled) };
            }
        }
    }

    fn phase_label(phase: ProgressPhase, language: Language) -> &'static str {
        let (japanese, english) = match phase {
            ProgressPhase::Analyzing => ("圧縮準備", "Preparing compression"),
            ProgressPhase::Compressing => ("FST並列圧縮", "Parallel FST compression"),
            ProgressPhase::Decompressing => (
                "FST並列解凍・検証",
                "Parallel FST extraction and verification",
            ),
            ProgressPhase::Verifying => ("FST並列検証", "Parallel FST verification"),
            ProgressPhase::ZipCompressing => ("ZIP圧縮", "ZIP compression"),
            ProgressPhase::ZipExtracting => ("ZIPエントリ並列展開", "Parallel ZIP extraction"),
            ProgressPhase::ZipVerifying => ("ZIPエントリ並列検証", "Parallel ZIP verification"),
            ProgressPhase::RecoveryCreating => ("復旧データ作成", "Creating recovery data"),
            ProgressPhase::RecoveryRepairing => ("書庫の修復", "Repairing archive"),
        };
        text(language, japanese, english)
    }

    unsafe fn selected_compression_level(parent: HWND) -> i32 {
        for (id, level) in [(ID_MODE_FAST, 0), (ID_MODE_DENSE, 12)] {
            let checked = unsafe {
                SendMessageW(
                    GetDlgItem(Some(parent), id).unwrap_or_default(),
                    BM_GETCHECK,
                    Some(WPARAM(0)),
                    Some(LPARAM(0)),
                )
            };
            if checked.0 as u32 == BST_CHECKED.0 {
                return level;
            }
        }
        1
    }

    fn compression_mode_label(level: i32, language: Language) -> &'static str {
        let (japanese, english) = match level {
            i32::MIN..=0 => ("最速（LZ4・固定チャンク）", "Fast (LZ4)"),
            1..=5 => ("バランス（Zstd-1）", "Balanced (Zstd-1)"),
            _ => ("高圧縮（Zstd-12）", "Dense (Zstd-12)"),
        };
        text(language, japanese, english)
    }

    fn compression_assessment(ratio: f64, language: Language) -> &'static str {
        let (japanese, english) = if ratio >= 0.98 {
            (
                "ほぼ圧縮不能（既に圧縮済み、暗号化済み、または高エントロピー）",
                "Nearly incompressible (already compressed, encrypted, or high entropy)",
            )
        } else if ratio >= 0.85 {
            ("低い（元データの重複が少ない可能性）", "Low")
        } else if ratio >= 0.55 {
            ("標準", "Normal")
        } else {
            ("高い", "High")
        };
        text(language, japanese, english)
    }

    unsafe fn confirm_overwrite(parent: HWND, path: &Path, language: Language) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let prompt = localized_format!(
            language,
            "{}\n\n既存ファイルを上書きしますか？",
            "{}\n\nOverwrite the existing file?",
            path.display()
        );
        let prompt = wide(&prompt);
        let answer = unsafe {
            MessageBoxW(
                Some(parent),
                PCWSTR(prompt.as_ptr()),
                w!("Fastener"),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
            )
        };
        if answer == IDYES {
            Ok(())
        } else {
            bail!(
                "{}",
                text(language, "操作をキャンセルしました", "Operation canceled")
            )
        }
    }

    unsafe fn get_secret(control: HWND) -> Result<Zeroizing<String>> {
        let length = unsafe { GetWindowTextLengthW(control) };
        let mut buffer = Zeroizing::new(vec![0u16; length as usize + 1]);
        let written = unsafe { GetWindowTextW(control, &mut buffer) };
        Ok(Zeroizing::new(String::from_utf16(
            &buffer[..written as usize],
        )?))
    }

    unsafe fn get_text(control: HWND) -> Result<String> {
        let length = unsafe { GetWindowTextLengthW(control) };
        let mut buffer = vec![0u16; length as usize + 1];
        let written = unsafe { GetWindowTextW(control, &mut buffer) };
        Ok(String::from_utf16_lossy(&buffer[..written as usize]))
    }

    unsafe fn set_status(parent: HWND, text: &str) {
        if let Ok(status) = unsafe { GetDlgItem(Some(parent), ID_STATUS) } {
            unsafe { set_text(status, text) };
        }
    }

    unsafe fn set_text(control: HWND, text: &str) {
        let text = wide(text);
        let _ = unsafe { SetWindowTextW(control, PCWSTR(text.as_ptr())) };
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn compressed_path(input: &Path) -> PathBuf {
        let mut output = input.as_os_str().to_owned();
        output.push(".fst");
        PathBuf::from(output)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ArchiveKind {
        Fastener,
        FastenerDirectory,
        Zip,
        Encrypted,
        Unknown,
    }

    fn detect_archive_kind(input: &Path, language: Language) -> Result<ArchiveKind> {
        let mut file = File::open(input).context(text(
            language,
            "書庫を開けません",
            "Could not open the archive",
        ))?;
        let mut magic = [0u8; 8];
        let magic_len = file.read(&mut magic).context(text(
            language,
            "書庫の形式を確認できません",
            "Could not identify the archive format",
        ))?;
        if magic_len == magic.len() && &magic == ENCRYPTED_MAGIC {
            return Ok(ArchiveKind::Encrypted);
        }
        if magic_len == magic.len() && &magic == b"FASTENR1" {
            return Ok(ArchiveKind::Fastener);
        }
        if magic_len == magic.len() && &magic == DIRECTORY_MAGIC {
            return Ok(ArchiveKind::FastenerDirectory);
        }
        if magic_len >= 4
            && matches!(
                &magic[..4],
                [b'P', b'K', 3, 4] | [b'P', b'K', 5, 6] | [b'P', b'K', 7, 8]
            )
        {
            return Ok(ArchiveKind::Zip);
        }
        Ok(ArchiveKind::Unknown)
    }

    fn restored_path(input: &Path) -> PathBuf {
        let base = if input
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("fst"))
        {
            input.with_extension("")
        } else {
            input.to_owned()
        };
        unique_output_path(&base, true)
    }

    fn zip_output_directory(input: &Path) -> PathBuf {
        let base = input.with_extension("");
        unique_output_path(&base, false)
    }

    fn unique_output_path(base: &Path, preserve_extension: bool) -> PathBuf {
        if !base.exists() {
            return base.to_owned();
        }
        let parent = base.parent().unwrap_or_else(|| Path::new("."));
        let stem = if preserve_extension {
            base.file_stem().unwrap_or(base.as_os_str())
        } else {
            base.file_name().unwrap_or(base.as_os_str())
        };
        for suffix in 2.. {
            let marker = format!(" ({suffix})");
            let mut name = stem.to_owned();
            name.push(marker);
            let mut candidate = parent.join(name);
            if preserve_extension && let Some(extension) = base.extension() {
                candidate.set_extension(extension);
            }
            if !candidate.exists() {
                return candidate;
            }
        }
        unreachable!()
    }

    fn human_bytes(bytes: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{bytes} B")
        } else {
            format!("{value:.2} {}", UNITS[unit])
        }
    }

    fn rate_labels(bytes: u64, elapsed: std::time::Duration, language: Language) -> String {
        if elapsed.is_zero() {
            return text(language, "計測中", "Measuring").to_owned();
        }
        let bytes_per_second = bytes as f64 / elapsed.as_secs_f64();
        let megabytes = bytes_per_second / 1_000_000.0;
        let gigabytes = bytes_per_second / 1_000_000_000.0;
        let megabits = bytes_per_second * 8.0 / 1_000_000.0;
        let gigabits = bytes_per_second * 8.0 / 1_000_000_000.0;
        format!(
            "{megabytes:.2} MB/s | {gigabytes:.3} GB/s | {megabits:.1} Mbps | {gigabits:.3} Gbps"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn masked_gui_controls_encrypt_decrypt_and_verify_with_real_worker() {
            use windows::Win32::UI::Controls::EM_GETPASSWORDCHAR;
            struct TestWindow(HWND);
            impl Drop for TestWindow {
                fn drop(&mut self) {
                    unsafe {
                        let _ = DestroyWindow(self.0);
                    }
                }
            }
            fn wait_for_worker(window: HWND) {
                let deadline = Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    let mut message = MSG::default();
                    if unsafe {
                        PeekMessageW(
                            &mut message,
                            Some(window),
                            WM_FASTENER_PROGRESS,
                            WM_FASTENER_FINISHED,
                            PM_REMOVE,
                        )
                    }
                    .as_bool()
                    {
                        // Consume actual worker messages without opening a modal error dialog in a test.
                        let payload = unsafe { Box::from_raw(message.lParam.0 as *mut UiMessage) };
                        if message.message == WM_FASTENER_FINISHED {
                            BUSY.store(false, Ordering::Release);
                            unsafe {
                                set_controls_enabled(window, true);
                            }
                            assert!(!payload.error, "{}", payload.text);
                            return;
                        }
                    } else {
                        assert!(Instant::now() < deadline, "GUI worker timed out");
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            }
            let temp = tempfile::tempdir().unwrap();
            let input = temp.path().join("日本語-input.txt");
            std::fs::write(&input, "GUI経由の暗号化テスト").unwrap();
            unsafe {
                // A real, hidden Win32 parent and controls; no desktop interaction required.
                let window = TestWindow(
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("STATIC"),
                        w!("Fastener test"),
                        WS_OVERLAPPEDWINDOW,
                        0,
                        0,
                        760,
                        680,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap(),
                );
                create_controls(window.0);
                let password = GetDlgItem(Some(window.0), ID_PASSWORD).unwrap();
                let confirm = GetDlgItem(Some(window.0), ID_PASSWORD_CONFIRM).unwrap();
                assert_ne!(SendMessageW(password, EM_GETPASSWORDCHAR, None, None).0, 0);
                assert_ne!(SendMessageW(confirm, EM_GETPASSWORDCHAR, None, None).0, 0);
                set_text(
                    GetDlgItem(Some(window.0), ID_PATH).unwrap(),
                    input.to_str().unwrap(),
                );
                set_text(password, "GUI test passphrase");
                set_text(confirm, "GUI test passphrase");
                let _ = SendMessageW(
                    GetDlgItem(Some(window.0), ID_ENCRYPT).unwrap(),
                    BM_SETCHECK,
                    Some(WPARAM(BST_CHECKED.0 as usize)),
                    Some(LPARAM(0)),
                );
                start_operation(window.0, Operation::Compress);
                assert!(BUSY.load(Ordering::Acquire));
                assert_eq!(GetWindowTextLengthW(password), 0);
                assert_eq!(GetWindowTextLengthW(confirm), 0);
                wait_for_worker(window.0);
                let archive = compressed_path(&input);
                assert_eq!(
                    detect_archive_kind(&archive, Language::English).unwrap(),
                    ArchiveKind::Encrypted
                );
                let restored = restored_path(&archive);
                set_text(
                    GetDlgItem(Some(window.0), ID_PATH).unwrap(),
                    archive.to_str().unwrap(),
                );
                set_text(password, "GUI test passphrase");
                start_operation(window.0, Operation::Decompress);
                wait_for_worker(window.0);
                assert_eq!(
                    std::fs::read(&restored).unwrap(),
                    std::fs::read(&input).unwrap()
                );
                set_text(password, "GUI test passphrase");
                start_operation(window.0, Operation::Verify);
                wait_for_worker(window.0);
                assert_eq!(GetWindowTextLengthW(password), 0);
                start_operation(window.0, Operation::RecoveryCreate);
                wait_for_worker(window.0);
                assert!(fastener::recovery_path(&archive).exists());
                let original = std::fs::read(&archive).unwrap();
                let mut broken = original.clone();
                broken[0] ^= 1;
                std::fs::write(&archive, broken).unwrap();
                set_text(password, "GUI test passphrase");
                start_operation(window.0, Operation::Repair);
                wait_for_worker(window.0);
                assert_eq!(
                    std::fs::read(fastener::repaired_path(&archive)).unwrap(),
                    original
                );
                assert_eq!(GetWindowTextLengthW(password), 0);
            }
        }

        #[test]
        fn output_paths_do_not_replace_the_input() {
            let input = Path::new(r"C:\data\sample.bin");
            assert_eq!(
                compressed_path(input),
                PathBuf::from(r"C:\data\sample.bin.fst")
            );
            assert_eq!(
                restored_path(Path::new(r"C:\data\sample.bin.fst")),
                PathBuf::from(r"C:\data\sample.bin")
            );
        }

        #[test]
        fn collisions_use_readable_names_without_fake_extensions() {
            let temp = tempfile::tempdir().unwrap();

            let original_file = temp.path().join("sample.bin");
            std::fs::write(&original_file, b"original").unwrap();
            assert_eq!(
                restored_path(&temp.path().join("sample.bin.fst")),
                temp.path().join("sample (2).bin")
            );
            std::fs::write(temp.path().join("sample (2).bin"), b"collision").unwrap();
            assert_eq!(
                restored_path(&temp.path().join("sample.bin.fst")),
                temp.path().join("sample (3).bin")
            );

            let original_folder = temp.path().join("folder");
            std::fs::create_dir(&original_folder).unwrap();
            assert_eq!(
                directory_bundle_output(&temp.path().join("folder.fst")),
                temp.path().join("folder (2)")
            );

            let zip_target = temp.path().join("archive");
            std::fs::create_dir(&zip_target).unwrap();
            assert_eq!(
                zip_output_directory(&temp.path().join("archive.zip")),
                temp.path().join("archive (2)")
            );
        }

        #[test]
        fn zip_is_detected_without_archive_loading() {
            let temp = tempfile::tempdir().unwrap();
            let zip = temp.path().join("large.zip");
            std::fs::write(&zip, b"PK\x03\x04not-a-fastener-archive").unwrap();
            assert_eq!(
                detect_archive_kind(&zip, Language::English).unwrap(),
                ArchiveKind::Zip
            );
        }

        #[test]
        fn directory_archive_is_detected() {
            let temp = tempfile::tempdir().unwrap();
            let archive = temp.path().join("folder.fst");
            std::fs::write(&archive, DIRECTORY_MAGIC).unwrap();
            assert_eq!(
                detect_archive_kind(&archive, Language::English).unwrap(),
                ArchiveKind::FastenerDirectory
            );
        }

        #[test]
        fn collision_names_are_language_independent() {
            let temp = tempfile::tempdir().unwrap();
            let original = temp.path().join("sample.bin");
            std::fs::write(&original, b"original").unwrap();
            assert_eq!(
                restored_path(&temp.path().join("sample.bin.fst")),
                temp.path().join("sample (2).bin")
            );
            assert_eq!(
                phase_label(ProgressPhase::Compressing, Language::English),
                "Parallel FST compression"
            );
        }

        #[test]
        fn gui_reserves_one_quarter_of_logical_processors() {
            assert_eq!(recommended_worker_threads(20), 15);
            assert_eq!(recommended_worker_threads(8), 6);
            assert_eq!(recommended_worker_threads(4), 3);
            assert_eq!(recommended_worker_threads(2), 1);
            assert_eq!(recommended_worker_threads(1), 1);
        }
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_app::run() {
        let message = format!(
            "Fastener GUI could not start.\nFastener GUIを起動できませんでした。\n\n{error:#}"
        );
        let message: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                None,
                windows::core::PCWSTR(message.as_ptr()),
                windows::core::w!("Fastener"),
                windows::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
}
