#!/usr/bin/env python3
"""Pure synthetic integrity tests; no world replay, optimizer or real snapshot."""
import math
import struct
import unittest
from check_halfspace_snapshot import IntegrityError, SCALAR_NAMES, bits, decode, descriptor_scalars, verify


def f32bits(v):
    return struct.unpack('<I',struct.pack('<f',v))[0]


def fixture():
    points=[(decode(str(f32bits(x)),32),.5,0.0) for x in [0,.06,.12]]
    radius=decode(str(f32bits(.04)),32)
    out=[]
    def put(tag,*values):
        out.append('\t'.join(map(str,(tag,)+values)))
    put('ENTRY',1,1,f32bits(1),f32bits(.04))
    put('PUBLIC_ENTRY_CONTEXT',0,0,f32bits(1/60),3)
    put('CORE',9,3,128,0,0)
    put('IDENTITY','Round219','dd6aa6bf7f749d53f8028d435e89c98d26d5c2b9425f83fb402898bf1ce809c9','a'*64,'budget_residual')
    for tag in ['PREDICTED','POSITION','ROLLBACK','FINAL_POSITION','VELOCITY','FINAL_VELOCITY']:
        for i,p in enumerate(points):
            put(tag,i,*(f32bits(v) for v in (p if 'VELOCITY' not in tag else (0,0,0))))
    residuals=[]
    for i in range(3):
        put('MASS',i,bits(1.0))
        put('DRIFT',i,bits(0.0))
    for k in range(9):
        put('Y',k,bits(0.0),bits(0.0),bits(1.0),bits(0.0))
    r=0
    for i in range(3):
        for j in range(i+1,3):
            delta=[points[i][k]-points[j][k] for k in range(3)]
            length=math.sqrt(sum(v*v for v in delta))
            normal=[v/length for v in delta]
            b=2*radius-length
            residuals.append(b)
            put('KIND',r,'pair',i,j)
            put('ROW',r,bits(b),bits(2.0),bits(0.0),bits(b))
            for k,v in enumerate(normal): put('COEFF',r,3*i+k,bits(v))
            for k,v in enumerate(normal): put('COEFF',r,3*j+k,bits(-v))
            r+=1
    order=sorted(range(3),key=lambda i:(-residuals[i],i))
    for r in order: put('TOP',r,bits(residuals[r]))
    put('SUMMARY',order[0],bits(max(residuals)),2,0,bits(0),bits(0),bits(0),bits(0),'true')
    for i in range(128): put('SWEEP',i+1,bits(max(residuals)),bits(0),bits(0))
    distance=min(abs(points[i][0]-points[j][0]) for i in range(3) for j in range(i+1,3))
    clearance=min(min(.5-abs(p[0]),.5-abs(p[2]),p[1],1-p[1])-radius for p in points)
    put('PUBLIC_FINAL',1,0,f32bits(1/60),bits(distance),bits(clearance))
    return ('\n'.join(out)+'\n').encode()


def wall_fixture():
    lines=fixture().decode().splitlines()
    output=[]
    for line in lines:
        if line.startswith('CORE\t'):
            line='CORE\t9\t6\t128\t0\t0'
        output.append(line)
    def put(tag,*values): output.append('\t'.join(map(str,(tag,)+values)))
    put('BODY',0,'Static','false',1)
    put('BOX',0,0,*(f32bits(v) for v in [0,-.05,0,0,0,0,1,1,1,1,.6,.05,.6]))
    radius=decode(str(f32bits(.04)),32)
    for i,x in enumerate([0,.06,.12]):
        x=decode(str(f32bits(x)),32)
        r=3+i
        put('KIND',r,'wall',i,0,1)
        put('WALL',r,*(bits(v) for v in [x,0,0,0,1,0,.5]))
        put('ROW',r,bits(radius-.5),bits(1.0),bits(0.0),bits(radius-.5))
        for k,a in enumerate([0.,1.,0.]): put('COEFF',r,3*i+k,bits(a))
    # Rows/kinds are indexed in stable order, independent of line interleaving.
    old_top=[line for line in output if line.startswith('TOP\t')]
    output=[line for line in output if not line.startswith('TOP\t')]
    output.extend(old_top)
    for r in range(3,6): put('TOP',r,bits(radius-.5))
    return ('\n'.join(output)+'\n').encode()


class SnapshotIntegrityTests(unittest.TestCase):
    def check(self,data):
        return verify(data,expected_particles=3,expected_walls=0)

    def test_valid_synthetic_walls(self):
        result=verify(wall_fixture(),expected_particles=3,expected_walls=1)
        self.assertEqual(result['rows'],6)

    def test_wall_metadata_corruption(self):
        data=wall_fixture().decode().replace('WALL\t3\t','WALL\t4\t',1).encode()
        with self.assertRaises(IntegrityError): verify(data,expected_particles=3,expected_walls=1)

    def test_box_scale_corruption(self):
        lines=wall_fixture().decode().splitlines()
        for i,line in enumerate(lines):
            if line.startswith('BOX\t'):
                fields=line.split('\t'); fields[10]=str(f32bits(2)); lines[i]='\t'.join(fields)
        with self.assertRaises(IntegrityError): verify(('\n'.join(lines)+'\n').encode(),expected_particles=3,expected_walls=1)

    def test_valid_synthetic_budget_failure(self):
        result=self.check(fixture())
        self.assertEqual(result['integrity'],'passed')
        self.assertTrue(result['not_a_feasibility_or_precision_result'])

    def corrupt(self,prefix,index,value):
        lines=fixture().decode().splitlines()
        for i,line in enumerate(lines):
            if line.startswith(prefix):
                fields=line.split('\t'); fields[index]=str(value); lines[i]='\t'.join(fields)
                break
        else: self.fail('missing synthetic tag')
        with self.assertRaises((IntegrityError,ValueError,IndexError)):
            self.check(('\n'.join(lines)+'\n').encode())

    def test_rollback_corruption(self): self.corrupt('ROLLBACK\t0\t',2,f32bits(.1))
    def test_reference_corruption(self): self.corrupt('Y\t0\t',3,bits(.001))
    def test_denominator_corruption(self): self.corrupt('ROW\t0\t',3,bits(3))
    def test_residual_corruption(self): self.corrupt('ROW\t0\t',5,bits(.1))
    def test_negative_multiplier(self): self.corrupt('ROW\t0\t',4,bits(-1))
    def test_nonfinite(self): self.corrupt('Y\t0\t',2,bits(float('nan')))
    def test_descriptor_scalar_name_corruption(self):
        rows=[[name,str(f32bits(0))] for name in sorted(SCALAR_NAMES)]
        self.assertEqual(len(descriptor_scalars(rows)),24)
        rows[0][0]='wrong_name'
        with self.assertRaises(IntegrityError): descriptor_scalars(rows)

    def test_descriptor_nonfinite(self):
        rows=[[name,str(f32bits(0))] for name in sorted(SCALAR_NAMES)]
        rows[0][1]=str(f32bits(float('inf')))
        with self.assertRaises(IntegrityError): descriptor_scalars(rows)

    def test_dt_corruption(self): self.corrupt('PUBLIC_ENTRY_CONTEXT\t',3,f32bits(.02))
    def test_step_corruption(self): self.corrupt('PUBLIC_ENTRY_CONTEXT\t',1,1)
    def test_sweep_budget_corruption(self): self.corrupt('CORE\t',3,129)
    def test_row_identity_corruption(self): self.corrupt('KIND\t0\t',4,2)
    def test_top_order_corruption(self): self.corrupt('TOP\t',1,2)
    def test_final_geometry_corruption(self): self.corrupt('PUBLIC_FINAL\t',4,bits(.9))
    def test_coefficient_corruption(self): self.corrupt('COEFF\t0\t0\t',3,bits(.5))
    def test_missing_row(self):
        data=b'\n'.join(line for line in fixture().split(b'\n') if not line.startswith(b'ROW\t0\t'))
        with self.assertRaises(IntegrityError): self.check(data)
    def test_duplicate_singleton(self):
        with self.assertRaises(IntegrityError): self.check(fixture()+b'ENTRY\t1\t1\t1065353216\t1025758986\n')


if __name__=='__main__': unittest.main()
