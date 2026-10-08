/*
 * crt-lite: brightness-dependent beams, phosphor masks and optional glow.
 * Copyright (C) 2026 Ygor Dreyer
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#pragma parameter BEAM_MIN "Beam width dark" 0.27 0.10 1.00 0.01
#pragma parameter BEAM_MAX "Beam width bright" 0.69 0.20 1.50 0.01
#pragma parameter H_SOFT "Horizontal softness" 0.50 0.00 1.00 0.05
#pragma parameter MASK_TYPE "Mask 0 off 1 RGB 2 MG" 1.0 0.0 2.0 1.0
#pragma parameter MASK_STRENGTH "Mask strength" 0.30 0.00 1.00 0.05
#pragma parameter BRIGHTNESS "Brightness boost" 1.25 0.50 2.00 0.05
#pragma parameter GLOW "Glow" 0.00 0.00 1.00 0.05
#pragma parameter VIGNETTE "Vignette" 0.10 0.00 1.00 0.05
#pragma parameter CURVATURE "Curvature" 0.00 0.00 0.20 0.01

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
COMPAT_ATTRIBUTE COMPAT_PRECISION vec4 OrigTexCoord;
uniform mat4 MVPMatrix;
uniform COMPAT_PRECISION vec2 TextureSize;
uniform COMPAT_PRECISION vec2 InputSize;
uniform COMPAT_PRECISION vec2 OrigTextureSize;
uniform COMPAT_PRECISION vec2 OrigInputSize;
COMPAT_VARYING COMPAT_PRECISION vec2 originalCoord;
COMPAT_VARYING COMPAT_PRECISION vec2 originalOrigin;
COMPAT_VARYING COMPAT_PRECISION vec2 sourceOrigin;

void main()
{
    gl_Position = MVPMatrix * VertexCoord;
    originalCoord = OrigTexCoord.xy;
    // Constant crop origins also support GB frames centred in the padded texture.
    originalOrigin = OrigTexCoord.xy * OrigTextureSize - VertexCoord.xy * OrigInputSize;
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
uniform COMPAT_COLOR sampler2D OrigTexture;
uniform COMPAT_PRECISION vec2 TextureSize;
uniform COMPAT_PRECISION vec2 InputSize;
uniform COMPAT_PRECISION vec2 OrigTextureSize;
uniform COMPAT_PRECISION vec2 OrigInputSize;
COMPAT_VARYING COMPAT_PRECISION vec2 originalCoord;
COMPAT_VARYING COMPAT_PRECISION vec2 originalOrigin;
COMPAT_VARYING COMPAT_PRECISION vec2 sourceOrigin;
#define Source Texture

#ifdef PARAMETER_UNIFORM
uniform COMPAT_COLOR float BEAM_MIN;
uniform COMPAT_COLOR float BEAM_MAX;
uniform COMPAT_COLOR float H_SOFT;
uniform COMPAT_COLOR float MASK_TYPE;
uniform COMPAT_COLOR float MASK_STRENGTH;
uniform COMPAT_COLOR float BRIGHTNESS;
uniform COMPAT_COLOR float GLOW;
uniform COMPAT_COLOR float VIGNETTE;
uniform COMPAT_PRECISION float CURVATURE;
#else
#define BEAM_MIN 0.27
#define BEAM_MAX 0.69
#define H_SOFT 0.50
#define MASK_TYPE 1.0
#define MASK_STRENGTH 0.30
#define BRIGHTNESS 1.25
#define GLOW 0.00
#define VIGNETTE 0.10
#define CURVATURE 0.00
#endif

COMPAT_COLOR vec3 originalSample(COMPAT_PRECISION vec2 centre)
{
    // Clamp local texel centres before restoring the crop's padding offset.
    COMPAT_PRECISION vec2 safeCentre = clamp(centre, vec2(0.5), OrigInputSize - vec2(0.5));
    return COMPAT_TEXTURE(OrigTexture, (originalOrigin + safeCentre) / OrigTextureSize).rgb;
}

void main()
{
    COMPAT_PRECISION vec2 p = originalCoord * OrigTextureSize - originalOrigin;
    COMPAT_PRECISION vec2 uv = p / OrigInputSize;
    if (CURVATURE > 0.0) {
        COMPAT_PRECISION vec2 d = uv - vec2(0.5);
        uv = vec2(0.5) + d * (1.0 + CURVATURE * dot(d, d));
        if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            FragColor = vec4(0.0, 0.0, 0.0, 1.0);
            return;
        }
        p = uv * OrigInputSize;
    }

    COMPAT_PRECISION float ix = floor(p.x);
    COMPAT_PRECISION float fx = p.x - ix - 0.5;
    COMPAT_PRECISION float x0 = ix + 0.5;
    COMPAT_PRECISION float x1 = x0 + sign(fx);
    COMPAT_COLOR float t = abs(fx) * H_SOFT;
    COMPAT_PRECISION float yy = p.y - 0.5;
    COMPAT_PRECISION float y0 = floor(yy);
    COMPAT_COLOR float f = yy - y0;

    // Exact centres make these four sharp-image fetches filter-independent.
    COMPAT_COLOR vec3 a = originalSample(vec2(x0, y0 + 0.5));
    COMPAT_COLOR vec3 b = originalSample(vec2(x1, y0 + 0.5));
    COMPAT_COLOR vec3 c = originalSample(vec2(x0, y0 + 1.5));
    COMPAT_COLOR vec3 d = originalSample(vec2(x1, y0 + 1.5));
    COMPAT_COLOR vec3 L0 = mix(a * a, b * b, t);
    COMPAT_COLOR vec3 L1 = mix(c * c, d * d, t);
    COMPAT_COLOR vec3 w0 = mix(vec3(BEAM_MIN), vec3(BEAM_MAX), L0);
    COMPAT_COLOR vec3 w1 = mix(vec3(BEAM_MIN), vec3(BEAM_MAX), L1);
    COMPAT_COLOR float nextDistance = 1.0 - f;
    COMPAT_COLOR vec3 weight0 = exp2(vec3(-f * f) / (w0 * w0));
    COMPAT_COLOR vec3 weight1 = exp2(vec3(-nextDistance * nextDistance) / (w1 * w1));
    COMPAT_COLOR vec3 tail0 = exp2(vec3(-1.0) / (w0 * w0));
    COMPAT_COLOR vec3 tail1 = exp2(vec3(-1.0) / (w1 * w1));
    // Zero tails keep pair changes continuous; 1 - tail >= 0.265 at width 1.50.
    weight0 = (weight0 - tail0) / max(vec3(1.0) - tail0, vec3(0.26));
    weight1 = (weight1 - tail1) / max(vec3(1.0) - tail1, vec3(0.26));
    COMPAT_COLOR vec3 col = L0 * weight0 + L1 * weight1;

    if (GLOW > 0.0) {
        // Source may be a cropped padded frame or the complete blur FBO.
        COMPAT_PRECISION vec2 centre = clamp(uv * InputSize, vec2(0.5), InputSize - vec2(0.5));
        COMPAT_COLOR vec3 g = COMPAT_TEXTURE(Source, (sourceOrigin + centre) / TextureSize).rgb;
        col += (g * g) * GLOW;
    }

    COMPAT_PRECISION vec2 pixel = floor(gl_FragCoord.xy);
    COMPAT_COLOR float dim = 1.0 - MASK_STRENGTH;
    COMPAT_COLOR vec3 mask = vec3(1.0);
    if (MASK_TYPE > 0.5 && MASK_TYPE < 1.5) {
        COMPAT_PRECISION float stripe = mod(pixel.x, 3.0);
        if (stripe < 1.0) mask = vec3(1.0, dim, dim);
        else if (stripe < 2.0) mask = vec3(dim, 1.0, dim);
        else mask = vec3(dim, dim, 1.0);
    } else if (MASK_TYPE >= 1.5) {
        if (mod(pixel.x, 2.0) < 1.0) mask = vec3(1.0, dim, 1.0);
        else mask = vec3(dim, 1.0, dim);
    }
    // Brightness boost compensates for the mask and the narrow dark beams.
    col = sqrt(clamp(col * mask * BRIGHTNESS, vec3(0.0), vec3(1.0)));
    if (VIGNETTE > 0.0) {
        COMPAT_COLOR float v = 16.0 * uv.x * (1.0 - uv.x) * uv.y * (1.0 - uv.y);
        COMPAT_COLOR float factor = 0.5 + 0.5 * v;
        col *= mix(1.0, factor, VIGNETTE);
    }
    FragColor = vec4(col, 1.0);
}
#endif
