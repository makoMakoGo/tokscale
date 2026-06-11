"use client";

import styled from "styled-components";
import { getSourceLogo } from "@/lib/constants";

interface SourceLogoProps {
  sourceId: string;
  height?: number;
  className?: string;
}

const StyledImg = styled.img<{ $height: number }>`
  border-radius: 2px;
  object-fit: contain;
  height: ${props => props.$height}px;
  width: auto;
  min-width: ${props => props.$height}px;
  max-width: ${props => props.$height}px;
  min-height: ${props => props.$height}px;
  max-height: ${props => props.$height}px;
`;

export function SourceLogo({ sourceId, height = 14, className = "" }: SourceLogoProps) {
  const src = getSourceLogo(sourceId);

  if (!src) {
    return <span className={className}>{sourceId}</span>;
  }

  return (
    <StyledImg
      src={src}
      alt={sourceId}
      $height={height}
      className={className}
    />
  );
}
