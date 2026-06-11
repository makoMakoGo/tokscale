import { isValidElement, type ReactElement } from "react";
import { describe, expect, it } from "vitest";
import { SourceLogo } from "../../src/components/SourceLogo";
import { getSourceLogo } from "../../src/lib/constants";
import { BASE_CLIENT_TYPES } from "../../src/lib/clientRegistry.generated";

type SourceLogoElementProps = {
  alt?: string;
  children?: string;
  className?: string;
  src?: string;
};

function renderSourceLogo(sourceId: string): ReactElement<SourceLogoElementProps> {
  const element = SourceLogo({ sourceId, height: 16, className: "source-logo" });

  expect(isValidElement(element)).toBe(true);
  return element as ReactElement<SourceLogoElementProps>;
}

describe("SourceLogo", () => {
  it("uses the shared logo registry for every supported base client", () => {
    for (const client of BASE_CLIENT_TYPES) {
      const element = renderSourceLogo(client);

      expect(element.type).not.toBe("span");
      expect(element.props.src).toBe(getSourceLogo(client));
      expect(element.props.alt).toBe(client);
      expect(element.props.className).toBe("source-logo");
    }
  });

  it("normalizes source ids before registry lookup", () => {
    const element = renderSourceLogo("CodeBuff");

    expect(element.type).not.toBe("span");
    expect(element.props.src).toBe(getSourceLogo("codebuff"));
    expect(element.props.alt).toBe("CodeBuff");
  });

  it("renders text for non-base variant ids", () => {
    const element = renderSourceLogo("cc-mirror/example");

    expect(element.type).toBe("span");
    expect(element.props.children).toBe("cc-mirror/example");
    expect(element.props.className).toBe("source-logo");
  });
});
