// Brand logo that follows the live light/dark theme.

import { useTheme } from "../hub/state/themeStore";
import gooseLogoLight from "../assets/goose-logo.png";
import gooseLogoDark from "../assets/goose-logo-dark.png";

interface LogoProps {
  /** img width+height in px (renders a square). Defaults to 32. */
  size?: number;
  /** Accessible alt text. Defaults to "Goose In A Pond". */
  alt?: string;
  /** Extra CSS class(es) applied to the <img> element. */
  className?: string;
  style?: React.CSSProperties;
}

export function Logo({ size = 32, alt = "Goose In A Pond", className, style }: LogoProps) {
  const { resolvedTheme } = useTheme();
  const src = resolvedTheme === "dark" ? gooseLogoDark : gooseLogoLight;

  return (
    <img
      src={src}
      alt={alt}
      width={size}
      height={size}
      className={className}
      style={{ objectFit: "contain", ...style }}
    />
  );
}
