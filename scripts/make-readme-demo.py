#!/usr/bin/env python3
"""Generate a GitHub GIF from native Weft UI and real isolated CLI results.

Requires macOS, a built dist/Weft.app, the local GGUF, and Pillow.
No desktop capture, personal index access, persistent preferences, or app installation.
Playback is edited and is not a latency measurement.
"""
from pathlib import Path
import json
import os
import plistlib
import shutil
import subprocess
import tempfile
import zipfile
from PIL import Image, ImageDraw, ImageFont, ImageFilter

REPO = Path(__file__).resolve().parents[1]
ASSETS = REPO / 'docs/assets'
WIDTH, HEIGHT = 1280, 900
FONT = '/System/Library/Fonts/Supplemental/Arial.ttf'
BOLD = '/System/Library/Fonts/Supplemental/Arial Bold.ttf'


def run(args, **kw):
    return subprocess.run(list(map(str, args)), check=True, **kw)


def font(size, bold=False):
    return ImageFont.truetype(BOLD if bold else FONT, size)


def compose(capture, scene):
    canvas = Image.new('RGB', (WIDTH, HEIGHT))
    pixels = canvas.load()
    for y in range(HEIGHT):
        for x in range(WIDTH):
            glow = max(0, 1 - ((x - 1050)**2 + (y - 120)**2)**0.5 / 1150)
            pixels[x, y] = (int(17 + 10*glow), int(18 + 5*glow), int(25 + 19*glow))
    draw = ImageDraw.Draw(canvas)
    draw.text((64, 38), 'Weft', font=font(25, True), fill='#cdc0ed')
    captions = [
        ('Find the file you remember.', 'Search filenames. Preview the source beside the results.'),
        ('Search the content you remember.', 'Meaning + content keywords, with the matching passage in view.'),
        ('Find the exact identifier.', 'A complete content match skips model inference.'),
        ('Preview documents, centered.', 'A compact page preview. Command-Y opens the full document.'),
        ('Start in your language.', 'Korean · English · Simplified Chinese · Japanese'),
    ]
    title, sub = captions[scene]
    draw.text((64, 90), title, font=font(44, True), fill='#f5f2fb')
    draw.text((66, 154), sub, font=font(21), fill='#aca9b8')
    panel = capture.convert('RGBA')
    # AppKit exports at backing scale; retain a readable 1040-point result panel.
    ratio = min(1, 1040 / panel.width, 556 / panel.height)
    panel = panel.resize((round(panel.width*ratio), round(panel.height*ratio)), Image.Resampling.LANCZOS)
    # Keep the small empty input at its native point size instead of stretching it.
    if capture.width >= 1300 and capture.height < 250:
        panel = capture.convert('RGBA').resize((680, round(capture.height/2)), Image.Resampling.LANCZOS)
    position = ((WIDTH-panel.width)//2, 235)
    shadow = Image.new('RGBA', canvas.size)
    sd = ImageDraw.Draw(shadow)
    sd.rounded_rectangle((position[0],position[1]+8,position[0]+panel.width,position[1]+panel.height+8), radius=20, fill=(0,0,0,150))
    canvas = Image.alpha_composite(canvas.convert('RGBA'), shadow.filter(ImageFilter.GaussianBlur(20)))
    canvas.alpha_composite(panel, position)
    draw = ImageDraw.Draw(canvas)
    draw.line((64, 824, 1216, 824), fill='#33313d', width=1)
    draw.text((64, 848), 'Local files. Local search.', font=font(19, True), fill='#cfc8df')
    draw.text((740, 851), 'Actual app UI · demo documents · edited playback', font=font(16), fill='#96919f')
    return canvas.convert('RGB')


def main():
    ASSETS.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='Weft-Demo-', dir='/tmp') as temporary:
        root = Path(temporary)
        app = root/'WeftReadme.app'
        (app/'Contents/MacOS').mkdir(parents=True)
        shutil.copytree(REPO/'dist/Weft.app/Contents/Resources', app/'Contents/Resources')
        info = plistlib.loads((REPO/'macos/Info.plist').read_bytes())
        info['CFBundleIdentifier'] = 'local.weft.readme.demo'
        (app/'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        docs=root/'Documents'; docs.mkdir()
        bodies={
            'Invoice approval.md': '# Invoice approval\n\nSupplier invoice INV-2041 uses approval route K7Q2LM.\nThe finance manager must approve payment before a supplier invoice is paid.\n\n청구서 승인 절차\n공급업체 청구서는 지급 전에 재무 담당자의 승인을 받아야 합니다.\n',
            'Database retry policy.md': '# Database retry policy\n\nRetry failed requests with exponential backoff and jitter.\nNever retry invalid credentials.\n',
            'Kyoto itinerary.md': '# Kyoto itinerary\n\nA quiet trip with hotel reservations, train routes and coffee stops.\n',
        }
        for name, body in bodies.items(): (docs/name).write_text(body)
        binary=REPO/'target/release/weft'
        prefix=[binary,'--db',root/'index.sqlite']
        run(prefix+['index',docs],capture_output=True,text=True)
        scenes=[('Invoice',0,['locate','Invoice','--top','5','--json']),
                ('청구서 승인',1,['search','청구서 승인','--top','3','--json']),
                ('INV-2041',1,['search','INV-2041','--top','3','--json'])]
        frames=[]
        for scene,(query,mode,args) in enumerate(scenes):
            result=f'results-{scene}.json'
            response=run(prefix+args,capture_output=True,text=True)
            hits=json.loads(response.stdout)
            assert hits and Path(hits[0]['path']).name=='Invoice approval.md'
            (root/result).write_text(response.stdout)
            # Edit typing and holds for legibility; never display invented latency.
            frames.append(dict(name=f'frame-{len(frames):03}',query='',mode=mode,result=None,duration=500,scene=scene))
            for i in range(1,len(query)+1):
                frames.append(dict(name=f'frame-{len(frames):03}',query=query[:i],mode=mode,result=None,duration=100,scene=scene))
            frames.append(dict(name=f'frame-{len(frames):03}',query=query,mode=mode,result=result,duration=3200,scene=scene))
        (root/'frames.json').write_text(json.dumps(frames,ensure_ascii=False))
        source=(REPO/'macos/Weft.swift').read_text().split('\nlet app = NSApplication.shared')[0]
        (root/'main.swift').write_text(source+'\n'+(REPO/'scripts/readme-demo.swift').read_text())
        sources=[root/'main.swift']+[REPO/'macos'/f for f in ['VoiceCapture.swift','Notes.swift','Onboarding.swift','IndexSettings.swift','QuickLinks.swift','Updates.swift','Brand.swift','WindowMotion.swift','FileThumbnailView.swift','AppLanguage.swift']]
        sources += sorted((REPO/'macos/Vendor/MarkdownEngine').rglob('*.swift'))
        args=['swiftc','-swift-version','5','-O','-target',os.uname().machine+'-apple-macosx14.0']
        for framework in ['Cocoa','Carbon','QuickLookUI','QuickLookThumbnailing','AVFoundation','SwiftUI']:args+=['-framework',framework]
        # First-page Office fixture, created after search indexing so demo rankings stay stable.
        with zipfile.ZipFile(docs/'Centered preview.docx','w',zipfile.ZIP_DEFLATED) as document:
            document.writestr('[Content_Types].xml','<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
            document.writestr('_rels/.rels','<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
            document.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:b/><w:sz w:val="38"/></w:rPr><w:t>Quarterly project report</w:t></w:r></w:p><w:p><w:r><w:t>A real Word document preview, centered inside the inspector.</w:t></w:r></w:p><w:p><w:r><w:t>Project milestones and next steps are visible without clipping the page.</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>')
        for directory in (REPO/'macos/Resources').glob('*.lproj'):
            shutil.copytree(directory,app/'Contents/Resources'/directory.name,dirs_exist_ok=True)
        run(args+sources+['-o',app/'Contents/MacOS/Weft'])
        run(['codesign','--force','--sign','-',app],capture_output=True)
        run([app/'Contents/MacOS/Weft',root])
        rendered=[compose(Image.open(root/(f['name']+'.png')),f['scene']) for f in frames]
        # One palette across all frames avoids flickering text and UI colors.
        sheet=Image.new('RGB',(WIDTH,HEIGHT*3))
        for scene in range(3):
            image=next(im for im,f in zip(rendered,frames) if f['scene']==scene and f['result'])
            sheet.paste(image,(0,HEIGHT*scene))
            image.save(ASSETS/f'weft-demo-{scene+1}.png')
        palette=sheet.quantize(colors=192,method=Image.Quantize.MEDIANCUT)
        indexed=[im.quantize(palette=palette,dither=Image.Dither.NONE) for im in rendered]
        indexed[0].save(ASSETS/'weft-search-demo.gif',save_all=True,append_images=indexed[1:],duration=[f['duration'] for f in frames],loop=0,optimize=True,disposal=2)
        compose(Image.open(root/'office-preview.png'),3).save(ASSETS/'weft-document-preview.png')
        compose(Image.open(root/'welcome-preview.png'),4).save(ASSETS/'weft-welcome-preview.png')
        (ASSETS/'weft-demo-poster.png').write_bytes((ASSETS/'weft-demo-2.png').read_bytes())
        print(json.dumps({'frames':len(frames),'duration_seconds':sum(f['duration'] for f in frames)/1000,'gif_bytes':(ASSETS/'weft-search-demo.gif').stat().st_size,'asset_directory':str(ASSETS)},indent=2))

if __name__=='__main__':main()
