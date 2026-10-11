#!/usr/bin/env python3
"""Native document extraction + real embedding/evidence integration; stdlib fixtures."""
import argparse
import json
from pathlib import Path
import struct
import sqlite3
import subprocess
import tempfile
import zipfile
import zlib


def png():
    def chunk(kind, data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(b'\x00\xff\x00\x00'))+chunk(b'IEND',b'')


def pdf():
    image = zlib.compress(b'\xff\x00\x00')
    text = b'BT /F1 12 Tf 50 700 Td (Database retry policy uses exponential backoff) Tj ET'
    objects = [b'<< /Type /Catalog /Pages 2 0 R >>',
               b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
               b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> /Contents 4 0 R >>',
               b'<< /Length '+str(len(text)).encode()+b' >>\nstream\n'+text+b'\nendstream',
               b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
               b'<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length '+str(len(image)).encode()+b' >>\nstream\n'+image+b'\nendstream']
    data=b'%PDF-1.5\n'; offsets=[]
    for i,obj in enumerate(objects,1):
        offsets.append(len(data));data+=f'{i} 0 obj\n'.encode()+obj+b'\nendobj\n'
    xref=len(data)
    data+=f'xref\n0 {len(objects)+1}\n'.encode()+b'0000000000 65535 f \n'
    data+=b''.join(f'{offset:010} 00000 n \n'.encode() for offset in offsets)
    data+=f'trailer\n<< /Size {len(objects)+1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode()
    return data


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--binary',default='target/release/myvec')
    args=parser.parse_args()
    binary=str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix='myvec-documents-') as tmp:
        root=Path(tmp).resolve(); docs=root/'docs';docs.mkdir();db=root/'index.sqlite'
        def archive(name, parts):
            with zipfile.ZipFile(docs/name,'w',zipfile.ZIP_DEFLATED) as file:
                for path,value in parts.items():file.writestr(path,value)
        archive('retry.docx',{'word/document.xml':'<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Database retry policy uses exponential backoff and jitter.</w:t></w:r></w:p><w:p><w:r><w:t>한글 재시도 문서 &amp; request deadlines</w:t></w:r></w:p></w:body></w:document>','word/media/image1.png':png()})
        archive('travel.pptx',{'ppt/presentation.xml':'<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="7" r:id="r2"/><p:sldId id="8" r:id="r1"/></p:sldIdLst></p:presentation>',
            'ppt/_rels/presentation.xml.rels':'<Relationships><Relationship Id="r1" Target="slides/slide1.xml"/><Relationship Id="r2" Target="slides/slide2.xml"/></Relationships>',
            'ppt/slides/slide1.xml':'<p:sld><a:p><a:t>Hotel reservations</a:t></a:p></p:sld>',
            'ppt/slides/slide2.xml':'<p:sld><a:p><a:t>Kyoto travel budget and train tickets</a:t></a:p></p:sld>',
            'ppt/media/image1.png':png()})
        archive('budget.xlsx',{'xl/workbook.xml':'<workbook xmlns:r="r"><sheets><sheet name="Budget" r:id="r1"/></sheets></workbook>',
            'xl/_rels/workbook.xml.rels':'<Relationships><Relationship Id="r1" Target="worksheets/sheet1.xml"/></Relationships>',
            'xl/sharedStrings.xml':'<sst><si><t>Kyoto travel budget</t></si></sst>',
            'xl/worksheets/sheet1.xml':'<worksheet><sheetData><row><c t="s"><v>0</v></c><c><v>1200</v></c></row></sheetData></worksheet>',
            'xl/media/image1.png':png()})
        archive('plan.odt',{'content.xml':'<office:document><text:p>Kyoto travel itinerary</text:p></office:document>','Pictures/image1.png':png()})
        (docs/'policy.pdf').write_bytes(pdf())
        def run(*argv):
            p=subprocess.run([binary,'--db',str(db),*map(str,argv)],capture_output=True,text=True,timeout=60)
            assert p.returncode==0,p.stderr
            return json.loads(p.stdout)
        images=root/'images'
        for file in sorted(docs.iterdir()):
            extracted=run('--model-file',root/'missing.gguf','extract',file,'--images-dir',images,'--json')
            assert extracted['text'] and extracted['locations']
            assert len(extracted['images'])==1
            output=Path(extracted['images'][0]['output_path'])
            assert output.read_bytes().startswith(b'\x89PNG')
            if file.suffix!='.pdf':assert output.read_bytes()==png()
        assert not db.exists(), 'extraction must not open an index or inference engine'
        p=subprocess.run([binary,'--db',str(db),'index',str(docs)],capture_output=True,text=True,timeout=60)
        assert p.returncode==0,p.stderr
        s=run('stats','--under',docs,'--kind','text','--json')
        assert s['scope']['files']==5 and s['scope']['chunks']>=5
        evidence=run('retrieve','database retry backoff','--under',docs,'--kind','text','--level','chunk','--top','5','--context-chars','4000','--json')
        assert any(h['path'].endswith('retry.docx') and h['source_state']=='verified' for h in evidence['hits'])
        assert any(h['path'].endswith('policy.pdf') and h['source_state']=='verified' for h in evidence['hits'])
        for hit in evidence['hits']:
            assert hit['line_kind']=='extracted_text'
            if hit['source_state']=='verified':assert hit['source_locations']
        # File-only evidence can reconstruct binary source text without a model.
        only=run('--model-file',root/'missing.gguf','retrieve','--file',docs/'retry.docx','--under',docs,'--kind','text','--context-chars','4000','--json')
        assert only['hits'] and any(h['source_state']=='verified' for h in only['hits'])
        # Simulate a legacy filename-only document, then verify content upgrade.
        with sqlite3.connect(db) as conn:
            conn.execute("UPDATE documents SET kind='path',hash='legacy-filename-hash' WHERE path=?",(str(docs/'retry.docx'),))
            conn.execute("UPDATE chunks SET input_hash=NULL WHERE doc_id=(SELECT id FROM documents WHERE path=?)",(str(docs/'retry.docx'),))
        p=subprocess.run([binary,'--db',str(db),'index',str(docs/'retry.docx')],capture_output=True,text=True,timeout=60)
        assert p.returncode==0,p.stderr
        with sqlite3.connect(db) as conn:
            assert conn.execute("SELECT kind FROM documents WHERE path=?",(str(docs/'retry.docx'),)).fetchone()[0]=='text'
        broken=docs/'corrupt.pdf';broken.write_bytes(b'not a PDF')
        p=subprocess.run([binary,'--db',str(db),'index',str(broken),'--all'],capture_output=True,text=True,timeout=60)
        assert p.returncode!=0 and 'files failed to index' in p.stderr,p.stderr
        with sqlite3.connect(db) as conn:
            assert conn.execute("SELECT kind FROM documents WHERE path=?",(str(broken),)).fetchone()[0]=='path'
        (docs/'policy.pdf').write_bytes(pdf()+b'\nchanged after index')
        stale=run('--model-file',root/'missing.gguf','retrieve','--file',docs/'retry.docx','--under',docs,'--kind','text','--context-chars','4000','--json')
        pdf_hit=next(h for h in stale['hits'] if h['path'].endswith('policy.pdf'))
        assert pdf_hit['source_state']=='changed_since_index' and 'context' not in pdf_hit
        print(json.dumps({'passed':True,'formats':['pdf','docx','pptx','xlsx','odt'],
                          'images_export_without_model_or_database':True,
                          'native_semantic_document_search':True,'verified_locations':True,
                          'legacy_filename_record_upgraded':True,'corrupt_document_filename_fallback':True,'binary_source_change_detected':True,'file_only_evidence_without_model':True},indent=2))


if __name__=='__main__':main()
