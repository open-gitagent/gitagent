import { readFile, writeFile, mkdir, stat } from "fs/promises";
import { join } from "path";
import { execFileSync } from "child_process";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import { capturePhotoSchema } from "./shared.js";

const PHOTOS_DIR = "memory/photos";
const INDEX_FILE = "memory/photos/INDEX.md";
const LATEST_FRAME_FILE = "memory/.latest-frame.jpg";

function slugify(text: string): string {
	return text
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "-")
		.replace(/^-|-$/g, "")
		.slice(0, 40);
}

export function createCapturePhotoTool(cwd: string): AgentTool<typeof capturePhotoSchema> {
	return {
		name: "capture_photo",
		label: "capture_photo",
		description:
			"Capture a photo from the webcam during a memorable moment. Reads the latest camera frame, saves it as a named photo in memory/photos/, updates the index, and commits to git.",
		parameters: capturePhotoSchema,
		execute: async (
			_toolCallId: string,
			{ reason }: { reason: string },
			signal?: AbortSignal,
		) => {
			if (signal?.aborted) throw new Error("Operation aborted");

			const framePath = join(cwd, LATEST_FRAME_FILE);

			// Check if frame file exists and isn't stale
			let frameStat;
			try {
				frameStat = await stat(framePath);
			} catch {
				return {
					content: [{ type: "text" as const, text: "No camera frame available. The webcam may not be active." }],
					details: undefined,
				};
			}

			const ageMs = Date.now() - frameStat.mtimeMs;
			if (ageMs > 5000) {
				return {
					content: [{ type: "text" as const, text: "No recent camera frame (camera may be off). Last frame is too stale to capture." }],
					details: undefined,
				};
			}

			// Read the frame
			const frameData = await readFile(framePath);

			// Build filename
			const now = new Date();
			const pad = (n: number) => String(n).padStart(2, "0");
			const datePart = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
			const timePart = `${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
			const slug = slugify(reason);
			const filename = `${datePart}_${timePart}_${slug}.jpg`;
			const photoRelPath = `${PHOTOS_DIR}/${filename}`;
			const photoAbsPath = join(cwd, photoRelPath);

			// Ensure photos directory exists
			await mkdir(join(cwd, PHOTOS_DIR), { recursive: true });

			// Write photo
			await writeFile(photoAbsPath, frameData);

			// Update INDEX.md
			const indexPath = join(cwd, INDEX_FILE);
			let indexContent = "";
			try {
				indexContent = await readFile(indexPath, "utf-8");
			} catch {
				indexContent = "# Memorable Moments\n\nPhotos captured during happy and memorable moments.\n\n";
			}
			const entry = `- **${datePart} ${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}** — ${reason} → [\`${filename}\`](${filename})\n`;
			indexContent += entry;
			await writeFile(indexPath, indexContent, "utf-8");

			// Git add + commit
			const commitMsg = `Capture moment: ${reason}`;
			try {
				execFileSync("git", ["add", photoRelPath, INDEX_FILE], { cwd, stdio: "pipe" });
				execFileSync("git", ["commit", "-m", commitMsg], {
					cwd,
					stdio: "pipe",
				});
			} catch (err: any) {
				const stderr = err.stderr?.toString() || "";
				return {
					content: [{ type: "text" as const, text: `Photo saved to ${photoRelPath} but git commit failed: ${stderr.trim() || "unknown error"}` }],
					details: undefined,
				};
			}

			return {
				content: [{ type: "text" as const, text: `Memorable moment captured! Photo saved to ${photoRelPath} and committed: "${commitMsg}"` }],
				details: undefined,
			};
		},
	};
}
