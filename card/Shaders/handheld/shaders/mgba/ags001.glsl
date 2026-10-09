/*
   AGS-001 shader
   A pristine recreation of the illuminated Game Boy Advance SP
   Author: endrift
   Slot adaptation: selectable three-column mask and mask strength.
   License: MPL 2.0

   This Source Code Form is subject to the terms of the Mozilla Public
   License, v. 2.0. If a copy of the MPL was not distributed with this
   file, You can obtain one at http://mozilla.org/MPL/2.0/. 
*/

#pragma parameter LCD_SCALE "LCD Mask Scale" 4.0 3.0 4.0 1.0
#pragma parameter MASK_STRENGTH "LCD Mask Strength" 1.0 0.0 1.0 0.05

#if defined(VERTEX)

#if __VERSION__ >= 130
#define COMPAT_VARYING out
#define COMPAT_ATTRIBUTE in
#define COMPAT_TEXTURE texture
#else
#define COMPAT_VARYING varying
#define COMPAT_ATTRIBUTE attribute 
#define COMPAT_TEXTURE texture2D
#endif

#ifdef GL_ES
#define COMPAT_PRECISION mediump
#else
#define COMPAT_PRECISION
#endif

COMPAT_ATTRIBUTE vec4 VertexCoord;
COMPAT_ATTRIBUTE vec4 COLOR;
COMPAT_ATTRIBUTE vec4 TexCoord;
COMPAT_VARYING vec4 COL0;
COMPAT_VARYING vec4 TEX0;

uniform mat4 MVPMatrix;
uniform COMPAT_PRECISION int FrameDirection;
uniform COMPAT_PRECISION int FrameCount;
uniform COMPAT_PRECISION vec2 OutputSize;
uniform COMPAT_PRECISION vec2 TextureSize;
uniform COMPAT_PRECISION vec2 InputSize;

// compatibility #defines
#define vTexCoord TEX0.xy
#define SourceSize vec4(TextureSize, 1.0 / TextureSize) //either TextureSize or InputSize
#define OutSize vec4(OutputSize, 1.0 / OutputSize)

void main()
{
    gl_Position = MVPMatrix * VertexCoord;
    TEX0.xy = TexCoord.xy;
}

#elif defined(FRAGMENT)

#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#define COMPAT_PRECISION mediump
#else
#define COMPAT_PRECISION
#endif

#if __VERSION__ >= 130
#define COMPAT_VARYING in
#define COMPAT_TEXTURE texture
out COMPAT_PRECISION vec4 FragColor;
#else
#define COMPAT_VARYING varying
#define FragColor gl_FragColor
#define COMPAT_TEXTURE texture2D
#endif

uniform COMPAT_PRECISION int FrameDirection;
uniform COMPAT_PRECISION int FrameCount;
uniform COMPAT_PRECISION vec2 OutputSize;
uniform COMPAT_PRECISION vec2 TextureSize;
uniform COMPAT_PRECISION vec2 InputSize;
uniform COMPAT_PRECISION vec2 OrigInputSize;
uniform sampler2D Texture;
COMPAT_VARYING vec4 TEX0;

// compatibility #defines
#define Source Texture
#define vTexCoord TEX0.xy

#define SourceSize vec4(TextureSize, 1.0 / TextureSize) //either TextureSize or InputSize
#define OutSize vec4(OutputSize, 1.0 / OutputSize)

#ifdef PARAMETER_UNIFORM
uniform COMPAT_PRECISION float LCD_SCALE;
uniform COMPAT_PRECISION float MASK_STRENGTH;
#else
#define LCD_SCALE 4.0
#define MASK_STRENGTH 1.0
#endif

void main()
{
	vec4 color = COMPAT_TEXTURE(Source, vTexCoord);
	vec2 original_coord = vTexCoord * TextureSize.xy / InputSize.xy * OrigInputSize.xy;
	color.rgb = pow(color.rgb, vec3(1.6));

	// A three-column panel needs all RGB columns, without the fourth grey column.
	float mask_scale = floor(LCD_SCALE + 0.5);
	int colorX = int(mod(original_coord.x * mask_scale, mask_scale));
	vec3 mask = vec3(0.4);
	if (colorX == 0) {
		mask = vec3(1.0, 0.2, 0.2);
	} else if (colorX == 1) {
		mask = vec3(0.2, 1.0, 0.2);
	} else if (colorX == 2) {
		mask = vec3(0.2, 0.2, 1.0);
	}
	float colorY = floor(mod(original_coord.y * mask_scale, mask_scale));
	if (colorY == mask_scale - 1.0) {
		mask *= 0.9;
	}
	color.rgb *= mix(vec3(1.0), mask, MASK_STRENGTH);

	color.a = 0.8;
	FragColor = color;
}
#endif
