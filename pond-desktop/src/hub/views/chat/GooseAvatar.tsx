import { Goose } from "../../../components/Goose";

interface GooseAvatarProps {
  size?: number;
  /** Drives the bird's animation — lets a caller show it working. */
  state?: "idle" | "working" | "listening";
}

/** The shared `<Goose />` in a fixed round badge, for placing beside a message. */
export function GooseAvatar({ size = 30, state = "idle" }: GooseAvatarProps) {
  return (
    <span
      className="ch-avatar"
      style={{ "--av-size": `${size}px` } as React.CSSProperties}
      aria-hidden="true"
    >
      <Goose state={state} size={size * 0.78} water={false} />
    </span>
  );
}
