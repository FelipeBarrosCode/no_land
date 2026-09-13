import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import { listRemoteUploadFolders, type RemoteFolderListing } from "../../lib/backend";
import type { RentedInstanceSummary } from "../../lib/types";

interface Props {
  instance: RentedInstanceSummary;
  onUpload: (instanceId: number, paths: string[], destination?: string) => Promise<void>;
  onClose: () => void;
}

export function InstanceUploadModal({ instance, onUpload, onClose }: Props) {
  const [pickerMode, setPickerMode] = useState<"files" | "folders">("files");
  const [destination, setDestination] = useState("Downloads");
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [dragActive, setDragActive] = useState(false);
  const [starting, setStarting] = useState(false);
  const destinationRef = useRef("Downloads");
  const startingRef = useRef(false);

  const startUpload = (paths: string[]) => {
    if (paths.length === 0 || startingRef.current) return;
    startingRef.current = true;
    setStarting(true);
    void onUpload(instance.instanceId, paths, destinationRef.current)
      .then(onClose)
      .catch(() => {
        startingRef.current = false;
        setStarting(false);
      });
  };

  const openNativePicker = async () => {
    if (startingRef.current) return;
    const selection = await open({
      title: pickerMode === "files" ? "Choose files to upload" : "Choose folders to upload",
      multiple: true,
      directory: pickerMode === "folders",
      recursive: pickerMode === "folders",
    });
    if (!selection) return;
    startUpload(Array.isArray(selection) ? selection : [selection]);
  };

  useEffect(() => {
    if (folderPickerOpen) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void getCurrentWindow().onDragDropEvent(({ payload }) => {
      if (disposed || startingRef.current) return;
      if (payload.type === "over") {
        setDragActive(true);
        return;
      }
      if (payload.type === "leave") {
        setDragActive(false);
        return;
      }
      if (payload.type === "drop" && payload.paths.length > 0) {
        setDragActive(false);
        startUpload(payload.paths);
      }
    }).then((removeListener) => {
      if (disposed) removeListener();
      else unlisten = removeListener;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [folderPickerOpen, instance.instanceId, onClose, onUpload]);

  return (
    <>
      <ModalFrame panelClassName="glass-panel pixel-frame max-w-3xl">
        <div className="flex shrink-0 items-start justify-between gap-4 border-b-2 border-[#3e4270] px-5 py-4">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.16em] text-neon-cyan">Direct SCP transfer</p>
            <h2 className="mt-1 font-display text-base text-white">Upload to {instance.label}</h2>
          </div>
          <Button variant="ghost" onClick={onClose} disabled={starting}>Close</Button>
        </div>

        <ModalBody className="space-y-4 px-5 py-4">
          <button
            type="button"
            className="flex w-full items-center justify-between gap-4 rounded border border-[#30365d] bg-[#11162a] px-4 py-3 text-left transition hover:border-neon-cyan disabled:opacity-50"
            onClick={() => setFolderPickerOpen(true)}
            disabled={starting}
          >
            <span>
              <span className="block font-display text-[10px] uppercase tracking-[0.14em] text-[#9ec4df]">Remote destination</span>
              <span className="mt-1 block font-mono text-sm text-white">{destination.startsWith("/") ? destination : `~/${destination}`}</span>
            </span>
            <span className="shrink-0 font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan">Choose folder &gt;</span>
          </button>

          <div className="flex rounded border border-[#30365d] bg-[#0b0f23] p-1" aria-label="Native picker type">
            <button
              type="button"
              className={`flex-1 rounded px-3 py-2 font-display text-[10px] uppercase tracking-[0.14em] transition ${pickerMode === "files" ? "bg-neon-cyan/15 text-neon-cyan" : "text-[#7183ac] hover:text-white"}`}
              onClick={() => setPickerMode("files")}
              disabled={starting}
              aria-pressed={pickerMode === "files"}
            >
              Select Files
            </button>
            <button
              type="button"
              className={`flex-1 rounded px-3 py-2 font-display text-[10px] uppercase tracking-[0.14em] transition ${pickerMode === "folders" ? "bg-neon-cyan/15 text-neon-cyan" : "text-[#7183ac] hover:text-white"}`}
              onClick={() => setPickerMode("folders")}
              disabled={starting}
              aria-pressed={pickerMode === "folders"}
            >
              Select Folders
            </button>
          </div>

          <button
            type="button"
            onClick={() => void openNativePicker()}
            disabled={starting}
            className={`flex min-h-72 w-full flex-col items-center justify-center rounded border-2 border-dashed px-8 py-12 text-center transition disabled:cursor-wait ${dragActive ? "border-neon-lime bg-neon-lime/10 shadow-[inset_0_0_30px_rgba(123,255,72,0.08)]" : "border-[#46517a] bg-[#080b18] hover:border-neon-cyan hover:bg-neon-cyan/5"}`}
          >
            <span className={`font-mono text-5xl ${dragActive ? "text-neon-lime" : "text-neon-cyan"}`} aria-hidden="true">↑</span>
            <p className="mt-5 font-display text-sm uppercase tracking-[0.16em] text-white">
              {starting ? "Starting transfer" : dragActive ? "Drop to upload" : `Drag and drop or click to choose ${pickerMode}`}
            </p>
            <p className="mt-3 max-w-md text-sm leading-6 text-[#8fa9c8]">Drag files and folders together, or click to open {navigator.userAgent.includes("Mac") ? "Finder" : "the native file explorer"}. Upload begins immediately after selection.</p>
            {starting && <p className="mt-4 animate-pulse font-mono text-xs text-[#ffd166]">Connecting to the remote instance...</p>}
          </button>
        </ModalBody>

        <div className="shrink-0 border-t-2 border-[#3e4270] px-5 py-4">
          <p className="text-xs text-[#7183ac]">No browse button, hashing, or cloud staging. Files move directly to the desktop user over SSH.</p>
        </div>
      </ModalFrame>

      {folderPickerOpen && (
        <RemoteFolderPicker
          instanceId={instance.instanceId}
          selectedDestination={destination}
          onSelect={(path) => {
            destinationRef.current = path;
            setDestination(path);
            setFolderPickerOpen(false);
          }}
          onClose={() => setFolderPickerOpen(false)}
        />
      )}
    </>
  );
}

interface RemoteFolderPickerProps {
  instanceId: number;
  selectedDestination: string;
  onSelect: (path: string) => void;
  onClose: () => void;
}

function joinRemotePath(parent: string, child: string): string {
  return parent === "/" ? `/${child}` : `${parent}/${child}`;
}

function pathName(path: string): string {
  if (path === "/") return "/";
  return path.replace(/\/+$/, "").split("/").pop() ?? path;
}

function RemoteFolderPicker({ instanceId, selectedDestination, onSelect, onClose }: RemoteFolderPickerProps) {
  const [rootPath, setRootPath] = useState<string | null>(null);
  const [homePath, setHomePath] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [childrenByPath, setChildrenByPath] = useState<Record<string, string[]>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [loadingPaths, setLoadingPaths] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);

  const rememberListing = (listing: RemoteFolderListing) => {
    setHomePath(listing.homePath);
    setChildrenByPath((current) => ({ ...current, [listing.path]: listing.folders }));
    return listing;
  };

  const loadRoot = async (path?: string) => {
    setError(null);
    setRootPath(null);
    try {
      const listing = rememberListing(await listRemoteUploadFolders(instanceId, path));
      setRootPath(listing.path);
      setExpanded((current) => ({ ...current, [listing.path]: true }));
      const initialSelection = selectedDestination.startsWith("/")
        ? selectedDestination
        : joinRemotePath(listing.homePath, selectedDestination);
      setSelectedPath(initialSelection);
    } catch (loadError) {
      setError(loadError instanceof Error ? loadError.message : String(loadError));
    }
  };

  useEffect(() => {
    void loadRoot();
  }, [instanceId]);

  const toggleFolder = async (path: string) => {
    if (expanded[path]) {
      setExpanded((current) => ({ ...current, [path]: false }));
      return;
    }
    setExpanded((current) => ({ ...current, [path]: true }));
    if (childrenByPath[path]) return;
    setLoadingPaths((current) => ({ ...current, [path]: true }));
    setError(null);
    try {
      rememberListing(await listRemoteUploadFolders(instanceId, path));
    } catch (loadError) {
      setExpanded((current) => ({ ...current, [path]: false }));
      setError(loadError instanceof Error ? loadError.message : String(loadError));
    } finally {
      setLoadingPaths((current) => ({ ...current, [path]: false }));
    }
  };

  const renderFolder = (path: string, depth: number) => {
    const children = childrenByPath[path] ?? [];
    const isExpanded = expanded[path] ?? false;
    const isSelected = selectedPath === path;
    return (
      <div key={path}>
        <div
          className={`flex items-center gap-2 rounded border px-2 py-1 ${isSelected ? "border-neon-cyan bg-neon-cyan/10" : "border-[#30365d] bg-[#11162a]"}`}
          style={{ marginLeft: `${depth * 12}px` }}
        >
          <button type="button" className="w-5 text-left text-[#7ab6ff]" onClick={() => void toggleFolder(path)} aria-label={`${isExpanded ? "Collapse" : "Expand"} ${pathName(path)}`}>
            {loadingPaths[path] ? "·" : isExpanded ? "▾" : "▸"}
          </button>
          <button type="button" className="flex min-w-0 flex-1 items-center gap-2 text-left" onClick={() => setSelectedPath(path)}>
            <span className={isSelected ? "text-neon-lime" : "text-[#7183ac]"} aria-hidden="true">{isSelected ? "●" : "○"}</span>
            <span className="truncate text-[1.15rem] text-[#d7e8ff]">{pathName(path)}/</span>
          </button>
        </div>
        {isExpanded && children.map((child) => renderFolder(joinRemotePath(path, child), depth + 1))}
      </div>
    );
  };

  return (
    <ModalFrame panelClassName="glass-panel pixel-frame max-w-3xl" zIndexClassName="z-[60]">
      <div className="flex shrink-0 items-start justify-between gap-4 border-b-2 border-[#3e4270] px-5 py-4">
        <div>
          <p className="font-display text-[10px] uppercase tracking-[0.16em] text-neon-cyan">Remote filesystem · desktop user</p>
          <h3 className="mt-1 font-display text-base text-white">Choose Destination Folder</h3>
        </div>
        <Button variant="ghost" onClick={onClose}>Back</Button>
      </div>

      <ModalBody className="space-y-4 px-5 py-4">
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="ghost" onClick={() => void loadRoot(homePath ?? undefined)} disabled={!homePath}>User Home</Button>
          <Button variant="ghost" onClick={() => void loadRoot("/")}>Filesystem Root</Button>
        </div>

        <div className="min-h-64 max-h-[45dvh] overflow-y-auto rounded border border-[#38466e] bg-[#080b18] p-2">
          {!rootPath ? (
            <p className="animate-pulse p-3 text-[1.15rem] text-[#ffd166]">Loading folder tree...</p>
          ) : (
            <div className="space-y-1">{renderFolder(rootPath, 0)}</div>
          )}
        </div>
        {error && <p className="rounded border border-red-500/30 bg-red-900/20 p-3 text-sm text-red-300">{error}</p>}
      </ModalBody>

      <div className="flex shrink-0 items-center justify-between gap-3 border-t-2 border-[#3e4270] px-5 py-4">
        <p className="min-w-0 truncate font-mono text-xs text-[#9ec0e4]">{selectedPath ?? "Select a folder"}</p>
        <div className="flex shrink-0 items-center gap-2">
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button disabled={!selectedPath} onClick={() => selectedPath && onSelect(selectedPath)}>Use This Folder</Button>
        </div>
      </div>
    </ModalFrame>
  );
}
