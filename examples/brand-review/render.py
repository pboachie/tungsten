"""Review assets: Pillow PNG rendering plus editable, self-contained SVGs.
Run: python render.py. No network calls or publication actions.
"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont
import base64, html, json, hashlib

SOURCE=Path(__file__).resolve().parent
ROOT=SOURCE.parents[1]/'assets/brand-review'
ROOT.mkdir(parents=True,exist_ok=True)
S=3
BG='#F8F7F2'; INK='#20231C'; MUTED='#62665C'; LINE='#DDDCD2'
INDIGO='#4F46E5'; GOLD='#8A6900'
FONT=Path('C:/Windows/Fonts/segoeui.ttf')
BOLD=Path('C:/Windows/Fonts/segoeuib.ttf')

class Art:
    def __init__(self,w,h,title):
        self.w,self.h=w,h
        self.im=Image.new('RGB',(w*S,h*S),BG); self.d=ImageDraw.Draw(self.im)
        self.svg=[f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img"><title>{html.escape(title)}</title><rect width="100%" height="100%" fill="{BG}"/>']
    def rect(self,x,y,w,h,fill,r=0,stroke=None):
        self.d.rounded_rectangle((x*S,y*S,(x+w)*S,(y+h)*S),radius=r*S,fill=fill,outline=stroke,width=S)
        self.svg.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill or "none"}"'+(f' stroke="{stroke}"' if stroke else '')+'/>')
    def line(self,xy,fill,width=1):
        self.d.line([(x*S,y*S) for x,y in xy],fill=fill,width=round(width*S),joint='curve')
        self.svg.append(f'<polyline points="{" ".join(f"{x},{y}" for x,y in xy)}" fill="none" stroke="{fill}" stroke-width="{width}"/>')
    def text(self,x,y,t,size=24,color=INK,bold=False,tracking=0):
        font=ImageFont.truetype(str(BOLD if bold else FONT),round(size*S))
        pos=x*S
        for ch in t:
            self.d.text((pos,y*S),ch,font=font,fill=color,anchor='ls')
            pos+=self.d.textlength(ch,font=font)+tracking*S
        self.svg.append(f'<text x="{x}" y="{y}" fill="{color}" font-family="Segoe UI, Arial, sans-serif" font-size="{size}" font-weight="{700 if bold else 400}" letter-spacing="{tracking}">{html.escape(t)}</text>')
    def image(self,x,y,w,h,path):
        im=Image.open(path).convert('RGB').resize((round(w*S),round(h*S)),Image.Resampling.LANCZOS)
        self.im.paste(im,(round(x*S),round(y*S)))
        b64=base64.b64encode(Path(path).read_bytes()).decode()
        self.svg.append(f'<image x="{x}" y="{y}" width="{w}" height="{h}" href="data:image/png;base64,{b64}"/>')
    def tungsten(self,x,y,scale=1):
        # Faithful rendering of the official SVG's geometry, colors, and declared Segoe UI fallback.
        # The editable SVG embeds the official file byte-for-byte, without modifying its identity.
        self.d.rounded_rectangle(((x+4*scale)*S,(y+8*scale)*S,(x+84*scale)*S,(y+88*scale)*S),radius=18*scale*S,fill=INDIGO)
        points=[(20,28),(30,68),(44,40),(58,68),(68,28)]
        xy=[((x+px*scale)*S,(y+py*scale)*S) for px,py in points]
        sw=7*scale*S
        self.d.line(xy,fill='white',width=round(sw),joint='curve')
        for px,py in xy:self.d.ellipse((px-sw/2,py-sw/2,px+sw/2,py+sw/2),fill='white')
        font=ImageFont.truetype(str(BOLD),round(44*scale*S)); pos=(x+100*scale)*S
        for ch in 'tungsten':
            self.d.text((pos,(y+64*scale)*S),ch,font=font,fill='#6366F1',anchor='ls')
            pos+=self.d.textlength(ch,font=font)-scale*S
        b64=base64.b64encode((SOURCE.parents[1]/'assets/logo.svg').read_bytes()).decode()
        self.svg.append(f'<image x="{x}" y="{y}" width="{360*scale}" height="{96*scale}" href="data:image/svg+xml;base64,{b64}"/>')
    def save(self,name):
        self.im.resize((self.w,self.h),Image.Resampling.LANCZOS).save(ROOT/f'{name}.png')
        (ROOT/f'{name}.svg').write_text('\n'.join(self.svg+['</svg>']),encoding='utf-8')

def base_banner(title,accent):
    a=Art(1500,500,title)
    a.rect(0,0,12,500,accent)
    a.line([(56,54),(1444,54)],LINE)
    a.line([(56,446),(1444,446)],LINE)
    # Quiet framing only; lower-left area deliberately contains no essential content.
    for yy,ww in [(136,154),(160,200),(184,114)]:a.rect(82,yy,ww,3,LINE)
    return a

a=base_banner('Tungsten — API descriptions. Agent-ready tools.',INDIGO)
a.tungsten(378,88,1.04)
a.text(384,245,'API descriptions.',56,bold=True)
a.text(384,313,'Agent-ready tools.',56,bold=True)
a.text(386,372,'pboachie.github.io/tungsten',24,MUTED)
a.rect(1168,166,230,148,'#EEEDF9',18)
a.text(1192,210,'SDKs',28,INDIGO,True)
a.text(1192,250,'CLI',28,INDIGO,True)
a.text(1192,290,'MCP',28,INDIGO,True)
a.save('tungsten-x-banner')

# Reusable template fields: edit these and rerun for a future reviewed release.
DEMO={'version':'v0.1.2','eyebrow':'A REAL TERMINAL WALKTHROUGH','title':['One API.','SDKs, CLI, MCP.'],
      'native':'Native OpenAPI: 7 SDK languages',
      'imports':'Imported SDK targets: TypeScript / Python / Rust'}
content_file=SOURCE/'thumbnail-content.json'
if content_file.exists():
    DEMO=json.loads(content_file.read_text(encoding='utf-8'))
else:
    content_file.write_text(json.dumps(DEMO,indent=2),encoding='utf-8')
a=Art(1280,720,'Tungsten '+DEMO['version']+' walkthrough: '+' '.join(DEMO['title']))
a.rect(0,0,12,720,INDIGO)
a.line([(64,176),(1216,176)],LINE)
a.tungsten(58,49,1.0)
a.rect(1038,65,178,52,'#EEEDF9',26)
a.text(1068,100,DEMO['version'],28,INDIGO,True)
a.text(64,227,DEMO['eyebrow'],17,INDIGO,True,1.8)
a.text(58,330,DEMO['title'][0],76,bold=True)
a.text(58,420,DEMO['title'][1],76,bold=True)
a.text(64,531,DEMO['native'],27,bold=True)
a.text(64,577,DEMO['imports'],25,MUTED)
a.line([(64,620),(1216,620)],LINE)
a.text(64,669,'pboachie.github.io/tungsten',23,MUTED)
a.text(1060,669,'WALKTHROUGH',15,INDIGO,True,1)
a.save('tungsten-demo-thumbnail')

# Review-only layout guides. X layouts vary; check the actual profile before publishing.
for name in ['tungsten-x-banner']:
    im=Image.open(ROOT/f'{name}.png').convert('RGBA'); overlay=Image.new('RGBA',im.size,(0,0,0,0)); d=ImageDraw.Draw(overlay)
    d.rectangle((350,70,1430,400),outline=(219,43,94,230),width=3)
    d.ellipse((40,310,330,600),fill=(219,43,94,25),outline=(219,43,94,200),width=3)
    d.text((360,410),'REVIEW GUIDE: essential content area / avatar reserve at lower left',font=ImageFont.truetype(str(FONT),18),fill=(160,25,66,255))
    Image.alpha_composite(im,overlay).convert('RGB').save(ROOT/f'{name}-safe-area.png')

