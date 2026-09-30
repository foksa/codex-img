# Prompting recipes

Adapted from the imagegen skill in pi-codex-image-gen (Apache-2.0).

## Contents
- [Generate](#generate)
- [Edit](#edit)
- [Iterating](#iterating)
- [Example prompts](#example-prompts)

## Generate
- **Photorealistic:** write it as if describing a real photo taken in the moment. Include lens, lighting and framing, plus real texture (pores, fabric wear, material grain). Avoid over-polished "render" language unless that's what the user wants.
- **Product mockup:** describe the product, materials and packaging. Ask for a clean silhouette and legible labels. Give label text verbatim, with typography.
- **UI mockup:** state the fidelity first (shippable mockup or low-fi wireframe), then layout, hierarchy and realistic UI elements. Avoid concept-art language.
- **Infographic or diagram:** define the audience and reading order, label every part explicitly, and require verbatim text. If the diagram must be exact, prefer code (SVG, HTML or a chart library) over image generation.
- **Logo or icon:** keep it simple and scalable, with a strong silhouette, balanced negative space and no decorative flourishes unless asked. App icons: square, and fill the whole canvas edge to edge with no rounded corners or border, because iOS and Android apply their own mask.
- **Ad or marketing:** write it like a creative brief: audience, vibe, scene, and the exact tagline if one should appear.
- **Illustration or story:** give concrete scene beats, one clear action per panel.
- **Stylized concept:** name the style cues, material finish and rendering approach (3D, painterly, clay, flat vector) without inventing new story elements.
- **Historical scene:** give the place and date, and keep clothing, props and setting accurate to the period.
- **Transparent asset (sticker, sprite, cutout):** use `-b transparent`. Ask for a single isolated subject, crisp edges, generous padding, and no shadow, floor, reflection or background.
- **Side-on game scenery (buildings, props for a low camera):** "front view at a slight angle" or "standing on a dock" gives a visible top surface, which in a pseudo-3D or side-scrolling game looks like the ground sloping up behind the object. Ask for: "seen perfectly straight on from the front at eye level, a flat front elevation with no top surfaces visible, its bottom edge a straight horizontal line, standing on nothing: no platform, no dock, no base, no ground". Boats and docks still tend to come with painted sea; remove it with `convert --key auto` (see SKILL.md).
- **A set of assets:** append one shared style sentence to every prompt (palette, pixel size or rendering, outline, lighting). That kept 80+ sprites of one game consistent without a style reference image. Review the set with `codex-img sheet`.

## Edit
- **Text localization:** change only the text. Keep layout, typography, spacing and hierarchy.
- **Keeping a person's identity:** lock face, body, pose, hair and expression, and change only the named elements. Match the lighting.
- **Precise object edit:** say exactly what to remove or replace, keep the surrounding texture and lighting, and leave everything else unchanged.
- **Lighting or weather:** change only light, shadows, atmosphere and precipitation. Keep geometry, framing and subject.
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

**Edit, keeping everything else**
```
Image 1 is the edit target. Change only the sky: make it a clear night with a
green aurora. Keep the fox, its pose, colours, the snow and the framing unchanged.
```

**Transparent sticker** (`-b transparent -o sticker.png`)
```
Die-cut sticker of a cheerful cartoon avocado giving a thumbs up, thick white
outline, flat colours. Single isolated subject, centered, generous padding,
no background, no shadow, no floor.
```
