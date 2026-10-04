/* =========================================================================
   Shared notes widget — the 0.4.x remake.

   Old behavior: one shared textarea bound to notes.txt.
   New behavior:
     • The widget header hosts + and − on the right (same header-row
       pattern as the clipboard widget's clear button).
     • + creates a numbered note button on the widget (1, 2, 3 …).
     • Clicking a button opens an INDEPENDENT note editor window
       (backend command open_note_window → `note-<num>` webview).
     • − toggles delete mode ON. Clicking a note button while delete
       mode is on removes that note (numbers of the others are kept —
       1 2 3 minus 2 leaves 1 and 3) and delete mode turns itself OFF;
       it must be re-enabled for every deletion.

   Used by BOTH surfaces that host widgets: the hushlight launcher and
   the widgets table. State syncs via the backend's `notes://changed`
   broadcast so an edit/deletion in one surface updates the other.
   ========================================================================= */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface NoteEntry {
  num: number;
  text: string;
}

let notes: NoteEntry[] = [];
let deleteMode = false;
let widgetEl: HTMLElement;
let listEl: HTMLElement;

/** Wire up the notes widget (ids: notes-widget, notes-add, notes-del,
 *  notes-list). Safe to call once per page. */
export async function initNotesWidget(): Promise<void> {
  widgetEl = document.getElementById("notes-widget")!;
  listEl = document.getElementById("notes-list")!;

  document.getElementById("notes-add")!.addEventListener("click", () => {
    // The notes://changed broadcast re-renders with the new button.
    void invoke<number>("note_create").catch(() => {});
  });

  document.getElementById("notes-del")!.addEventListener("click", () => {
    // − toggles delete mode; it is NOT a delete button itself.
    setDeleteMode(!deleteMode);
  });

  // Any create/delete (from this surface or the other one) re-renders.
  listen<NoteEntry[]>("notes://changed", (e) => {
    notes = e.payload;
    render();
  });

  // The widgets table's webview stays alive while hidden — a stale delete
  // mode must not survive a close/reopen of the table.
  listen("table://widgets-shown", () => setDeleteMode(false));

  await reload();
}

async function reload() {
  try {
    notes = await invoke<NoteEntry[]>("notes_list");
  } catch {
    notes = [];
  }
  render();
}

function setDeleteMode(on: boolean) {
  deleteMode = on;
  widgetEl.classList.toggle("delete-mode", on);
  document.getElementById("notes-del")!.classList.toggle("active", on);
  render();
}

async function onNoteClick(num: number) {
  if (deleteMode) {
    try {
      await invoke("note_delete", { num });
    } catch {}
    // One deletion per arming — delete mode always turns off afterwards.
    setDeleteMode(false);
  } else {
    try {
      await invoke("open_note_window", { num });
    } catch {}
  }
}

function render() {
  listEl.innerHTML = "";
  const sorted = [...notes].sort((a, b) => a.num - b.num);

  if (sorted.length === 0) {
    const empty = document.createElement("div");
    empty.className = "notes-empty";
    empty.textContent = deleteMode ? "Nothing to delete" : "No notes";
    listEl.appendChild(empty);
    return;
  }

  for (const note of sorted) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "note-btn";
    btn.textContent = String(note.num);
    btn.title = deleteMode ? `Delete note ${note.num}` : `Open note ${note.num}`;
    btn.addEventListener("click", () => void onNoteClick(note.num));
    listEl.appendChild(btn);
  }
}
