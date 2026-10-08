/*
 * crt-lite-glow: a five-tap source-resolution blur for crt-lite's glow.
 * Copyright (C) 2026 Ygor Dreyer
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma slot_filter linear

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
#define COMPAT_PRECISION highp
#else
#define COMPAT_PRECISION mediump
#endif
#define COMPAT_COLOR mediump
precision COMPAT_PRECISION float;
#else
#define COMPAT_PRECISION
#define COMPAT_COLOR
#endif

#if defined(VERTEX)
#if __VERSION__ >= 130
#define COMPAT_ATTRIBUTE in
#define COMPAT_VARYING out
#else
#define COMPAT_ATTRIBUTE attribute
#define COMPAT_VARYING varying
#endif

COMPAT_ATTRIBUTE vec4 VertexCoord;
COMPAT_ATTRIBUTE COMPAT_PRECISION vec4 TexCoord;
uniform mat4 MVPMatrix;
uniform COMPAT_PRECISION vec2 TextureSize;
uniform COMPAT_PRECISION vec2 InputSize;
COMPAT_VARYING COMPAT_PRECISION vec2 sourceCoord;
COMPAT_VARYING COMPAT_PRECISION vec2 sourceOrigin;

void main()
{
    gl_Position = MVPMatrix * VertexCoord;
    sourceCoord = TexCoord.xy;
    sourceOrigin = TexCoord.xy * TextureSize - VertexCoord.xy * InputSize;
}

#elif defined(FRAGMENT)
#if __VERSION__ >= 130
#define COMPAT_VARYING in
#define COMPAT_TEXTURE texture
out vec4 FragColor;
#else
#define COMPAT_VARYING varying
#define COMPAT_TEXTURE texture2D
#define FragColor gl_FragColor
#endif

uniform COMPAT_COLOR sampler2D Texture;
uniform COMPAT_PRECISION vec2 TextureSize;
uniform COMPAT_PRECISION vec2 InputSize;
COMPAT_VARYING COMPAT_PRECISION vec2 sourceCoord;
COMPAT_VARYING COMPAT_PRECISION vec2 sourceOrigin;
#define Source Texture

COMPAT_COLOR vec3 blurSample(COMPAT_PRECISION vec2 centre)
{
    COMPAT_PRECISION vec2 safeCentre = clamp(centre, vec2(0.5), InputSize - vec2(0.5));
    return COMPAT_TEXTURE(Source, (sourceOrigin + safeCentre) / TextureSize).rgb;
}

void main()
{
    COMPAT_PRECISION vec2 p = sourceCoord * TextureSize - sourceOrigin;
    // Normalized cross kernel: centre 0.40, four bilinear arms 0.15 each at 1.25 texels.
    // Away from edges, effective weights are 0.40 at the centre, 0.1125 at
    // each axial +/-1 texel and 0.0375 at each axial +/-2 texels (sum 1.0).
    COMPAT_COLOR vec3 col = blurSample(p) * 0.40;
    col += blurSample(p + vec2(1.25, 0.0)) * 0.15;
    col += blurSample(p - vec2(1.25, 0.0)) * 0.15;
    col += blurSample(p + vec2(0.0, 1.25)) * 0.15;
    col += blurSample(p - vec2(0.0, 1.25)) * 0.15;
    FragColor = vec4(col, 1.0);
}
#endif
