import clsx from "clsx";

const SOCIAL_LINKS = [
  {
    label: "X / @felipavav",
    url: "https://x.com/felipavav",
    ariaLabel: "Open Felipe's profile on X",
  },
  {
    label: "Discord",
    url: "https://discord.gg/Vqwxsfk3u4",
    ariaLabel: "Join the Noland Discord server",
  },
] as const;

interface Props {
  className?: string;
}

export function SocialLinks({ className }: Props) {
  return (
    <div className={clsx("flex flex-wrap items-center gap-2", className)}>
      {SOCIAL_LINKS.map((link) => (
        <a
          key={link.url}
          className="inline-flex items-center justify-center border border-[#495188] bg-transparent px-3 py-2 font-display text-[12px] uppercase tracking-[0.12em] text-[#b9caf0] shadow-[0_0_0_2px_#090a17,inset_0_0_0_1px_#11152f] transition duration-100 hover:border-[#61f7ff] hover:bg-[#10152f] hover:text-[#84fbff]"
          href={link.url}
          target="_blank"
          rel="noreferrer"
          aria-label={link.ariaLabel}
        >
          {link.label}
        </a>
      ))}
    </div>
  );
}
