/* =========================================================================
   Shared localization (i18n) for all windows.
   English is the default (also the fallback for unknown keys/values);
   Russian ("ru") ships alongside. The chosen language is persisted in
   settings.json (persist::Settings.language, default "en") and broadcast
   on the existing settings://changed event — the same plumbing the
   clock-format / desktop-grid settings use, so every open window flips
   live, exactly like a theme change.

   How strings bind:
   - Static HTML: data-i18n="key" (textContent), data-i18n-ph="key"
     (placeholder), data-i18n-title="key" (title attribute).
   - Dynamic TS strings: t("key") at build time (context menus, toasts
     and empty states are rebuilt on open, so they always re-translate).
   - Windows that render strings OUTSIDE of those two mechanisms (the
     note title, the "80 ms" suffix) listen for the i18n:changed DOM
     event dispatched here after every real language switch.
   Language names in the picker stay in their own language (English /
   Русский) — that is standard picker behavior, not a miss.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Lang = "en" | "ru";

// ===== English (source of truth — every key MUST exist here) =====
const EN: Record<string, string> = {
  // --- settings table ---
  "st.title": "Settings",
  "st.close": "Close",
  "st.appearance": "Appearance",
  "st.theme": "Theme",
  "st.language": "Language",
  "st.iconRecolor": "Icon Recoloring",
  "st.tables": "Tables",
  "st.winHold": "Win hold delay",
  "st.holdHint":
    "How long Win must be held before the tables appear. A quick tap still opens hushlight.",
  "st.ms": "ms",
  "st.widgets": "Widgets",
  "st.clipboard": "Clipboard",
  "st.notes": "Notes",
  "st.sysmon": "System Monitor",
  "st.volume": "Volume",
  "st.brightness": "Brightness",
  "st.system": "System",
  "st.clockFormat": "Clock format",
  "st.pieClock": "Pie hub clock",
  "st.maintenance": "Maintenance",
  "st.resetConfig": "Reset config (restarts the app clean)",
  "st.reset": "Reset",
  "st.resetTitle": "Delete all config files and restart",
  "st.clearIconCache": "Clear icon cache",
  "st.clear": "Clear",
  "st.clearTitle": "Drop cached file icons (they are re-extracted on demand)",
  "st.cleared": "Cleared",
  "st.exit": "Exit Hush_UI",
  "st.exitTitle": "Restore the native Windows shell and exit",

  // --- launcher ---
  "ln.apps": "Apps",
  "ln.noWindows": "No open windows",
  "ln.exitTitle": "Exit Hush_UI (restore Windows shell)",
  "ln.settingsTitle": "Settings",
  "ln.searchPh": "Hushlight",
  "ln.clipboard": "Clipboard",
  "ln.notes": "Notes",
  "ln.newNote": "New note",
  "ln.deleteMode": "Delete mode",
  "ln.system": "System",
  "ln.volume": "Volume",
  "ln.master": "Master",
  "ln.appVolumes": "App volumes",
  "ln.refreshApps": "Refresh app list",
  "ln.noApps": "No apps playing audio",
  "ln.runPh": "Enter command…",
  "ln.admin": "Admin",
  "ln.cancel": "Cancel",
  "ln.ok": "OK",
  "ln.nothingFound": "Nothing found",
  "ln.noHistory": "No history",
  "ln.screenshot": "Screenshot (click to re-copy)",
  "ln.new": "New",
  "ln.folder": "Folder",
  "ln.file": "File",
  "ln.refresh": "Refresh",
  "ln.newFolder": "New folder",
  "ln.folderName": "Folder name",
  "ln.newFile": "New file",
  "ln.extPh": "name.extension",

  // --- desktop table ---
  "dk.title": "Desktop",
  "dk.close": "Close",
  "dk.empty": "Desktop folder is empty",
  "dk.open": "Open",
  "dk.pin": "Pin to top",
  "dk.unpin": "Unpin",
  "dk.rename": "Rename",
  "dk.delete": "Delete",
  "dk.new": "New",
  "dk.folder": "Folder",
  "dk.file": "File",
  "dk.refresh": "Refresh",
  "dk.renameTitle": "Rename",
  "dk.newName": "New name",
  "dk.cancel": "Cancel",
  "dk.ok": "OK",
  "dk.newFolder": "New folder",
  "dk.folderName": "Folder name",
  "dk.newFile": "New file",
  "dk.extPh": "name.extension",

  // --- taskbar ---
  "tb.endTask": "End task",

  // --- widgets table ---
  "wg.title": "Widgets",
  "wg.close": "Close",
  "wg.system": "System",
  "wg.volume": "Volume",
  "wg.master": "Master",
  "wg.apps": "App volumes",
  "wg.refreshApps": "Refresh app list",
  "wg.noApps": "No apps playing audio",
  "wg.brightness": "Brightness",
  "wg.dim": "Dim",
  "wg.dimHint": "Not running as administrator — screen dimming might not work on system apps.",
  "wg.hushTable": "Hush table",
  "wg.noHistory": "No history",
  "wg.screenshot": "Screenshot (click to re-copy)",

  // --- hushlight ---
  "hl.searchPh": "Hushlight",
  "hl.nothingFound": "Nothing found",

  // --- pie picker wedges ---
  "tp.hushlight": "Hushlight",
  "tp.desktop": "Desktop",
  "tp.taskbarLine": "Taskbar line",
  "tp.widgets": "Widgets",
  "tp.settings": "Settings",

  // --- tutorial ---
  "tu.text":
    "To start the menu, hold windows key, to choose a table hover over it and let go of the windows key",
  "tu.understood": "Understood",
  "tu.fakewin": "Settings",

  // --- note window ---
  "nt.title": "Note",
  "nt.close": "Close",
  "nt.ph": "Write something…",
};

// ===== Russian — missing keys fall back to English =====
const RU: Record<string, string> = {
  // --- settings table ---
  "st.title": "Настройки",
  "st.close": "Закрыть",
  "st.appearance": "Внешний вид",
  "st.theme": "Тема",
  "st.language": "Язык",
  "st.iconRecolor": "Перекраска значков",
  "st.tables": "Столы",
  "st.winHold": "Задержка удержания Win",
  "st.holdHint":
    "Как долго нужно удерживать Win, прежде чем появятся столы. Короткое нажатие по-прежнему открывает Hushlight.",
  "st.ms": "мс",
  "st.widgets": "Виджеты",
  "st.clipboard": "Буфер обмена",
  "st.notes": "Заметки",
  "st.sysmon": "Системный монитор",
  "st.volume": "Громкость",
  "st.brightness": "Яркость",
  "st.system": "Система",
  "st.clockFormat": "Формат часов",
  "st.pieClock": "Часы в центре пирога",
  "st.maintenance": "Обслуживание",
  "st.resetConfig": "Сбросить конфиг (перезапустит приложение с нуля)",
  "st.reset": "Сбросить",
  "st.resetTitle": "Удалить все файлы конфигурации и перезапустить",
  "st.clearIconCache": "Очистить кэш значков",
  "st.clear": "Очистить",
  "st.clearTitle": "Удалить кэшированные значки (они извлекаются заново по требованию)",
  "st.cleared": "Очищено",
  "st.exit": "Выйти из Hush_UI",
  "st.exitTitle": "Вернуть оболочку Windows и выйти",

  // --- launcher ---
  "ln.apps": "Приложения",
  "ln.noWindows": "Нет открытых окон",
  "ln.exitTitle": "Выйти из Hush_UI (вернуть оболочку Windows)",
  "ln.settingsTitle": "Настройки",
  "ln.searchPh": "Hushlight",
  "ln.clipboard": "Буфер обмена",
  "ln.notes": "Заметки",
  "ln.newNote": "Новая заметка",
  "ln.deleteMode": "Режим удаления",
  "ln.system": "Система",
  "ln.volume": "Громкость",
  "ln.master": "Общий",
  "ln.appVolumes": "Громкость приложений",
  "ln.refreshApps": "Обновить список",
  "ln.noApps": "Нет приложений со звуком",
  "ln.runPh": "Введите команду…",
  "ln.admin": "Админ",
  "ln.cancel": "Отмена",
  "ln.ok": "OK",
  "ln.nothingFound": "Ничего не найдено",
  "ln.noHistory": "Нет истории",
  "ln.screenshot": "Скриншот (нажмите, чтобы скопировать снова)",
  "ln.new": "Создать",
  "ln.folder": "Папка",
  "ln.file": "Файл",
  "ln.refresh": "Обновить",
  "ln.newFolder": "Новая папка",
  "ln.folderName": "Имя папки",
  "ln.newFile": "Новый файл",
  "ln.extPh": "имя.расширение",

  // --- desktop table ---
  "dk.title": "Рабочий стол",
  "dk.close": "Закрыть",
  "dk.empty": "Папка рабочего стола пуста",
  "dk.open": "Открыть",
  "dk.pin": "Закрепить сверху",
  "dk.unpin": "Открепить",
  "dk.rename": "Переименовать",
  "dk.delete": "Удалить",
  "dk.new": "Создать",
  "dk.folder": "Папка",
  "dk.file": "Файл",
  "dk.refresh": "Обновить",
  "dk.renameTitle": "Переименовать",
  "dk.newName": "Новое имя",
  "dk.cancel": "Отмена",
  "dk.ok": "OK",
  "dk.newFolder": "Новая папка",
  "dk.folderName": "Имя папки",
  "dk.newFile": "Новый файл",
  "dk.extPh": "имя.расширение",

  // --- taskbar ---
  "tb.endTask": "Снять задачу",

  // --- widgets table ---
  "wg.title": "Виджеты",
  "wg.close": "Закрыть",
  "wg.system": "Система",
  "wg.volume": "Громкость",
  "wg.master": "Общий",
  "wg.apps": "Громкость приложений",
  "wg.refreshApps": "Обновить список",
  "wg.noApps": "Нет приложений со звуком",
  "wg.brightness": "Яркость",
  "wg.dim": "Затемнение",
  "wg.dimHint": "Не запущено от имени администратора — затемнение может не работать для системных приложений.",
  "wg.hushTable": "Стол Hush",
  "wg.noHistory": "Нет истории",
  "wg.screenshot": "Скриншот (нажмите, чтобы скопировать снова)",

  // --- hushlight ---
  "hl.searchPh": "Hushlight",
  "hl.nothingFound": "Ничего не найдено",

  // --- pie picker wedges ---
  "tp.hushlight": "Hushlight",
  "tp.desktop": "Рабочий стол",
  "tp.taskbarLine": "Панель задач",
  "tp.widgets": "Виджеты",
  "tp.settings": "Настройки",

  // --- tutorial ---
  "tu.text":
    "Чтобы открыть меню, удерживайте клавишу Win. Чтобы выбрать стол, наведите на него курсор и отпустите клавишу Win",
  "tu.understood": "Понятно",
  "tu.fakewin": "Настройки",

  // --- note window ---
  "nt.title": "Заметка",
  "nt.close": "Закрыть",
  "nt.ph": "Напишите что-нибудь…",
};

let current: Lang = "en";

export function getLang(): Lang {
  return current;
}

/// Translate a key. Unknown keys return the English string (or the key
/// itself if even English lacks it — surfaces dictionary gaps in dev).
export function t(key: string): string {
  if (current === "ru") {
    const ru = RU[key];
    if (ru !== undefined) return ru;
  }
  return EN[key] ?? key;
}

function normalize(lang: string | undefined | null): Lang {
  return lang === "ru" ? "ru" : "en";
}

/// Apply a language to the DOM: every data-i18n* attribute in the page.
/// Safe to call repeatedly (idempotent).
export function applyI18n(lang: Lang): void {
  current = normalize(lang);
  const root = document.documentElement;
  root.lang = current;
  root.dataset.lang = current;

  for (const el of Array.from(document.querySelectorAll("[data-i18n]"))) {
    el.textContent = t(el.getAttribute("data-i18n")!);
  }
  for (const el of Array.from(document.querySelectorAll("[data-i18n-ph]"))) {
    (el as HTMLInputElement).placeholder = t(el.getAttribute("data-i18n-ph")!);
  }
  for (const el of Array.from(document.querySelectorAll("[data-i18n-title]"))) {
    el.setAttribute("title", t(el.getAttribute("data-i18n-title")!));
  }
}

/// Startup: load the persisted language, apply it, then follow live
/// settings://changed broadcasts (emitted by save_settings). Fires an
/// "i18n:changed" DOM event whenever the language actually switches so
/// windows can re-render strings built outside data-i18n (note title,
/// "80 ms" suffix, …).
export async function initI18n(): Promise<Lang> {
  try {
    const s = await invoke<{ language?: string }>("load_settings");
    applyI18n(normalize(s.language));
  } catch {
    applyI18n("en");
  }

  void listen<{ language?: string }>("settings://changed", (e) => {
    const next = normalize(e.payload?.language);
    if (next !== current) {
      applyI18n(next);
      window.dispatchEvent(new CustomEvent("i18n:changed", { detail: current }));
    }
  });

  return current;
}
