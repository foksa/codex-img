# Prompting recipes

Adapted from the imagegen skills in pi-codex-image-gen and OpenAI Codex (both Apache-2.0).

## Contents
- [Structure](#structure)
- [How much to add](#how-much-to-add)
- [Composition](#composition)
- [Text in the image](#text-in-the-image)
- [Input images](#input-images)
- [Generate](#generate)
- [Edit](#edit)
- [Iterating](#iterating)
- [Example prompts](#example-prompts)

## Structure
Order: scene/background → subject → key details → constraints → intended use. A short prompt can be one paragraph. For anything with several requirements, write short labeled lines instead, using only the ones that help:

```
Asset type: <where it will be used: app icon, landing page hero, game sprite>
Primary request: <the user's request>
Input images: <Image 1: role; Image 2: role>
Scene/background: <setting>
Subject: <main subject>
Style/medium: <photo, flat illustration, 3D render, pixel art>
Composition/framing: <close-up, wide, top-down; placement>
Lighting/mood: <lighting and mood>
Color palette: <palette>
Materials/textures: <surface detail>
Text (verbatim): "<exact text>"
Constraints: <must keep / must have>
Avoid: <what must not appear>
```

`Scene/background` describes what's painted; it's unrelated to `-b`, which controls whether the file has real transparency.

## How much to add
- If the request is already specific, keep every requirement and only restructure it. Don't add creative choices.
- If it's vague, add only what clearly helps: framing, lighting, intended use, practical layout, and enough scene detail to make the request concrete.
- Never add characters, props, brand names, slogans, palettes or story beats that weren't implied.
- The examples below are complete recipes, not the amount of detail to add to every request.
- If a missing detail would make the result unusable (the exact text, which image is the edit target), ask. Otherwise proceed.

## Composition
- Name the framing and viewpoint (close-up, wide, top-down, eye level) when it matters.
- If the image needs room for headline copy or UI, ask for usable negative space. Don't pick a side (left/right) unless the layout around it calls for one.
- For people, say how much of the body is in frame and what they look at or hold: "full body visible", "looking down at the book", "hands gripping the handlebars".

## Text in the image
- Put the exact words in quotes (or ALL CAPS), and say font style, size, colour and placement.
- Ask for "verbatim, no extra text". For a tagline, say "render it exactly once".
- Spell unusual words or names letter by letter ("K-V-A-S-I-R") if they must be right.
- Check the spelling in the result. If text keeps coming out wrong, generate without it and add the text in code.

## Input images
- `-i` works both for edits and for references; the prompt decides which. Don't assume every input is the thing to edit.
- Label each input by index and role: "Image 1: edit target; Image 2: style reference only".
- Images given only for style, composition or mood mean a new image: describe the new subject and say what to take from each reference.
- When the user wants an existing image kept and only part of it changed, it's an edit: "change only X; keep Y unchanged".

## Generate
- **Photorealistic:** write it as if describing a real photo taken in the moment. Include lens, lighting and framing, plus real texture (pores, fabric wear, material grain). Avoid over-polished "render" language unless that's what the user wants.
- **Product mockup:** describe the product, materials and packaging. Ask for a clean silhouette and legible labels. Give label text verbatim, with typography.
- **Website image (hero, section, blog header):** say which one it is, and ask for negative space where page copy will sit. Usually "no text, no logos".
- **UI mockup:** state the fidelity first (shippable mockup or low-fi wireframe), then layout, hierarchy and realistic UI elements. Avoid concept-art language. For a wireframe: "low-fi grayscale wireframe, labelled blocks, no colour, no real photos", and list the sections in order.
- **Infographic, diagram or slide:** define the audience and reading order, give the real labels and numbers verbatim, and ask for readable type and generous whitespace (avoid tiny text). For science or teaching visuals, add the lesson's goal and "scientifically accurate". If it must be exact, prefer code (SVG, HTML or a chart library) over image generation.
- **Logo or icon:** keep it simple and scalable, with a strong silhouette, balanced negative space and no decorative flourishes unless asked. "Flat colours, no gradients, no mockup, no 3D" keeps a logo usable. App icons: square, and fill the whole canvas edge to edge with no rounded corners or border, because iOS and Android apply their own mask.
- **Ad or marketing:** write it like a creative brief: audience, vibe, scene, and the exact tagline if one should appear.
- **Illustration or story:** give concrete scene beats, one clear action per panel, and the panel layout ("4 equal vertical panels").
- **Stylized concept:** name the style cues, material finish and rendering approach (3D, painterly, clay, flat vector) without inventing new story elements.
- **Historical scene:** give the place and date, and keep clothing, props and setting accurate to the period ("no modern objects").
- **Transparent asset (sticker, sprite, cutout):** use `-b transparent`. Ask for a single isolated subject, crisp edges, generous padding, and no shadow, floor, reflection or background.
- **Side-on game scenery (buildings, props for a low camera):** "front view at a slight angle" or "standing on a dock" gives a visible top surface, which in a pseudo-3D or side-scrolling game looks like the ground sloping up behind the object. Ask for: "seen perfectly straight on from the front at eye level, a flat front elevation with no top surfaces visible, its bottom edge a straight horizontal line, standing on nothing: no platform, no dock, no base, no ground". Boats and docks still tend to come with painted sea; remove it with `convert --key auto` (see SKILL.md).
- **Tileable texture:** "seamless tileable texture, no focal point, even lighting". Check it by placing copies side by side. For a panorama that must wrap left to right, use `codex-img tile` instead.
- **A set of assets:** append one shared style sentence to every prompt (palette, pixel size or rendering, outline, lighting). That kept 80+ sprites of one game consistent without a style reference image. Review the set with `codex-img sheet`.
- **The same character in a new scene:** pass an earlier image of the character with `-i` as the anchor, and say "same character; don't redesign it; keep face, proportions, outfit and palette", then describe the new scene and action.

## Edit
- **Text localization:** change only the text. Keep layout, typography, spacing and hierarchy. List each old → new string.
- **Keeping a person's identity:** lock face, body, pose, hair and expression, and change only the named elements. Match the lighting.
- **Precise object edit:** say exactly what to remove or replace, keep the surrounding texture and lighting, and leave everything else unchanged.
- **Lighting or weather:** change only light, shadows, atmosphere and precipitation. Keep geometry, framing and subject.
- **Background removal:** use `-b transparent` with the photo as `-i`, and ask to keep the subject's edges and any label text exactly, with no halo and no restyling. Check that the result really has transparency; if a background was painted in instead, remove it with `convert --key` (see SKILL.md).
- **Style transfer:** say which style cues to take (palette, texture, brushwork) and add "no extra elements".
- **Compositing:** refer to inputs by index, say what moves where, and match perspective, scale and lighting.
- **Sketch to render:** keep the layout, proportions and perspective of the sketch, and add materials and lighting only.

## Iterating
- Start with a clean base prompt, then change one thing per round.
- Pass the last good output back with `-i` rather than regenerating from scratch.
- Restate the things that must not change on every round, because models drift.
- If a result misreads the request, check `revisedPrompt` in the JSON output and make that part of the prompt more explicit.

## Example prompts

**App icon**
```
App Store icon, square. A single white paper plane mid-flight on a smooth
blue-to-violet gradient that fills the entire canvas edge to edge, no rounded
corners, no border. Minimal, flat, soft long shadow. No text. Strong silhouette
that reads at 32px.
```

**Product photo**
```
Photorealistic studio product photo for an online shop. A matte black ceramic
coffee mug on a light oak table, soft window light from the left, shallow depth
of field, 50mm lens. Label on the mug reads "DAWN" in a thin sans-serif, white,
centered, verbatim with no extra text.
```

**Landing page hero** (labeled lines)
```
Asset type: landing page hero background, wide landscape
Primary request: minimal abstract background with a soft gradient and subtle paper texture
Style/medium: matte, softly rendered abstract illustration
Composition/framing: wide, with calm usable negative space for a headline
Color palette: restrained warm neutrals
Avoid: text, logos, focal objects
```

**Slide**
```
Asset type: pitch-deck slide, 16:9 landscape
Primary request: one slide titled "Market Opportunity"
Subject: TAM/SAM/SOM concentric circles, plus a small bar chart of growth from 2022 to 2026
Style/medium: clean modern slide, white background, crisp sans-serif type
Text (verbatim): "Market Opportunity", "TAM: $42B", "SAM: $8.7B", "SOM: $340M"
Constraints: readable labels, clear hierarchy, polished spacing
Avoid: clip art, stock photos, decorative clutter, extra text
```

**Edit, keeping everything else**
```
Image 1 is the edit target. Change only the sky: make it a clear night with a
green aurora. Keep the fox, its pose, colours, the snow and the framing unchanged.
```

**Same character, new scene** (`-i hero-anchor.png`)
```
Image 1: the character anchor; don't redesign the character.
Same young forest hero, now gently helping a frightened squirrel out of a fallen
tree in a snowy forest after a storm. Same children's book watercolour style as
Image 1. Keep facial features, proportions, outfit and colour palette. No text.
```

**Transparent sticker** (`-b transparent -o sticker.png`)
```
Die-cut sticker of a cheerful cartoon avocado giving a thumbs up, thick white
outline, flat colours. Single isolated subject, centered, generous padding,
no background, no shadow, no floor.
```
