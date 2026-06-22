import clsx from "clsx";

type IconName = "moonlight" | "server" | "play" | "settings" | "help" | "destroy" | "copy" | "close";

interface Props {
  icon: IconName;
  className?: string;
}

const palette = ["transparent", "#10131f", "#44d6ff", "#7cff47", "#ff4fbc", "#f4f8ff"];

const sprites: Record<IconName, number[]> = {
  moonlight: [
    0, 0, 3, 3, 3, 0,
    0, 3, 5, 5, 2, 0,
    3, 5, 5, 2, 0, 0,
    3, 5, 2, 0, 0, 0,
    3, 2, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0
  ],
  server: [
    2, 2, 2, 2, 2, 2,
    5, 5, 5, 5, 5, 5,
    2, 2, 2, 2, 2, 2,
    5, 5, 5, 5, 5, 5,
    2, 2, 4, 2, 2, 2,
    5, 5, 5, 5, 5, 5
  ],
  play: [
    0, 3, 0, 0, 0, 0,
    0, 3, 3, 0, 0, 0,
    0, 3, 3, 3, 0, 0,
    0, 3, 3, 3, 3, 0,
    0, 3, 3, 3, 0, 0,
    0, 3, 3, 0, 0, 0
  ],
  settings: [
    0, 2, 0, 2, 0, 0,
    2, 5, 5, 5, 2, 0,
    0, 5, 1, 5, 0, 0,
    2, 5, 5, 5, 2, 0,
    0, 2, 0, 2, 0, 0,
    0, 0, 0, 0, 0, 0
  ],
  help: [
    0, 0, 5, 5, 0, 0,
    0, 5, 0, 0, 5, 0,
    0, 0, 0, 0, 5, 0,
    0, 0, 0, 5, 0, 0,
    0, 0, 5, 0, 0, 0,
    0, 0, 0, 0, 0, 0
  ],
  destroy: [
    0, 0, 5, 5, 0, 0,
    0, 5, 5, 5, 5, 0,
    5, 2, 2, 2, 2, 5,
    5, 2, 2, 2, 2, 5,
    5, 2, 2, 2, 2, 5,
    5, 5, 5, 5, 5, 5
  ],
  copy: [
    0, 5, 5, 5, 5, 0,
    0, 5, 2, 2, 5, 0,
    5, 5, 2, 2, 5, 5,
    5, 2, 2, 2, 2, 5,
    5, 2, 2, 2, 2, 5,
    5, 5, 5, 5, 5, 5
  ],
  close: [
    5, 0, 0, 0, 0, 5,
    0, 5, 0, 0, 5, 0,
    0, 0, 5, 5, 0, 0,
    0, 0, 5, 5, 0, 0,
    0, 5, 0, 0, 5, 0,
    5, 0, 0, 0, 0, 5
  ],
};
export function SpriteIcon({ icon, className }: Props) {
  const sprite = sprites[icon];

  return (
    <div
      className={clsx("sprite-icon grid h-6 w-6 grid-cols-6 gap-[1px] bg-[#0a0e1f] p-[1px]", className)}
      aria-hidden="true"
    >
      {sprite.map((colorIndex, index) => (
        <span
          key={`${icon}-${index}`}
          style={{ backgroundColor: palette[colorIndex] }}
          className="block h-[3px] w-[3px]"
        />
      ))}
    </div>
  );
}
