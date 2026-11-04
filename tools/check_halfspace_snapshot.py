#!/usr/bin/env python3
"""Validate a Round219 snapshot without solving or replaying its physical world.

Exact bits: entry/rollback, reference A^T lambda, aa, stored row residual,
physical displacement and drift. Cross-language libm/reduction summaries use
abs=1e-12, rel=2e-12, explicitly unrelated to the physical 1e-7m certificate.
Intermediate sweep states are not captured: only their structure/finiteness and
last-sweep consistency can be independently checked, not all 128 trajectories.
"""
import argparse
import hashlib
import json
import math
import struct
from pathlib import Path


class IntegrityError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise IntegrityError(message)


def decode(value, width=64):
    bits = int(value)
    require(0 <= bits < 2 ** width, 'invalid bit integer')
    result = struct.unpack('<d' if width == 64 else '<f', bits.to_bytes(width // 8, 'little'))[0]
    require(math.isfinite(result), 'nonfinite encoded float')
    return result


def bits(value):
    return struct.unpack('<Q', struct.pack('<d', value))[0]


def bits32(value):
    return struct.unpack('<I', struct.pack('<f', value))[0]


def exact(actual, encoded, label):
    require(bits(actual) == int(encoded), label + ' bit mismatch')


def close(actual, encoded, label):
    target = decode(encoded)
    require(math.isclose(actual, target, rel_tol=2e-12, abs_tol=1e-12), label + ' numerical mismatch')


def serial_sum(values):
    result = 0.0
    for value in values:
        result += value
    return result


SCALAR_NAMES = {'spacing','radius','rest_density','viscosity','surface_tension','vorticity','drag','flow_coupling','emit_rate','emit_radius','emit_spread','foam_speed','foam_rate','contact_friction','bubble_rate','bubble_radius','bubble_buoyancy','bubble_drag','bubble_expansion','bubble_max_radius','bubble_wake','bubble_compressibility','bubble_pressure','bubble_momentum_coupling'}


def descriptor_scalars(rows):
    values = {}
    for row in rows:
        require(len(row) == 2 and row[0] not in values, 'descriptor scalar arity/duplicate')
        values[row[0]] = decode(row[1], 32)
    require(set(values) == SCALAR_NAMES, 'descriptor scalar name set')
    return values


def verify(data, expected_particles=125, expected_walls=6):
    text = data.decode('utf-8')
    lines = text.splitlines()
    tags = {}
    for line in lines:
        fields = line.split('\t')
        require(fields[0] != '', 'empty tag')
        tags.setdefault(fields[0], []).append(fields[1:])

    def one(tag, length=None):
        values = tags.get(tag, [])
        require(len(values) == 1, 'singleton ' + tag)
        require(length is None or len(values[0]) == length, 'arity ' + tag)
        return values[0]

    def indexed(tag, count, length):
        values = tags.get(tag, [])
        require(len(values) == count, 'count ' + tag)
        require(all(len(row) == length and int(row[0]) == i for i, row in enumerate(values)), 'order/arity ' + tag)
        return values

    entry = one('ENTRY', 4)
    step, calls = map(int, entry[:2])
    mass, radius = decode(entry[2], 32), decode(entry[3], 32)
    require(mass > 0 and radius > 0 and 1 <= step <= 120 and step == calls, 'entry state')
    context = one('PUBLIC_ENTRY_CONTEXT', 4)
    n = int(context[3])
    require(n == expected_particles and int(context[0]) + 1 == step, 'entry step/count')
    require(decode(context[1], 32) == 0.0 and int(context[2]) == bits32(1/60), 'entry dt/accumulator')
    core = one('CORE', 5)
    dimension, count, sweeps, updates, decreases = map(int, core)
    require(dimension == 3*n and sweeps == 128 and 0 <= decreases <= updates <= 128*count, 'core counters')
    require(count == n*(n-1)//2 + n*expected_walls and count <= 32768, 'complete graph count')
    identity = one('IDENTITY', 4)
    require(identity[0] == 'Round219' and identity[3] == 'budget_residual', 'identity/reason')
    for value in identity[1:3]:
        require(len(value) == 64 and all(c in '0123456789abcdef' for c in value), 'source hash')
    require(identity[1] == 'dd6aa6bf7f749d53f8028d435e89c98d26d5c2b9425f83fb402898bf1ce809c9', 'reference source')
    vectors = {}
    for tag in ['PREDICTED', 'POSITION', 'VELOCITY', 'ROLLBACK', 'FINAL_POSITION', 'FINAL_VELOCITY']:
        rows = indexed(tag, n, 4)
        vectors[tag] = [[decode(v, 32) for v in row[1:]] for row in rows]
    require(tags['PREDICTED'] == tags['ROLLBACK'], 'non-atomic rollback')
    masses = [decode(row[1]) for row in indexed('MASS', n, 2)]
    require(all(m > 0 and m == mass for m in masses), 'mass mismatch')
    rows = indexed('ROW', count, 5)
    coefficients = [[] for _ in rows]
    last_row = -1
    for row in tags.get('COEFF', []):
        require(len(row) == 3, 'coefficient arity')
        r, i = map(int, row[:2])
        require(0 <= r < count and r >= last_row and 0 <= i < dimension, 'coefficient order/index')
        coefficients[r].append((i, decode(row[2])))
        last_row = r
    kinds = tags.get('KIND', [])
    require(len(kinds) == count and all(int(k[0]) == i for i,k in enumerate(kinds)), 'kind count/order')
    pair_count = n*(n-1)//2
    pair_ids = [(i,j) for i in range(n) for j in range(i+1,n)]
    wall_records = tags.get('WALL', [])
    require(len(wall_records) == n*expected_walls, 'wall metadata count')
    registered_boxes = [
        ([0,-.05,0],[.6,.05,.6]), ([0,1.05,0],[.6,.05,.6]),
        ([-.55,.5,0],[.05,.6,.6]), ([.55,.5,0],[.05,.6,.6]),
        ([0,.5,-.55],[.6,.6,.05]), ([0,.5,.55],[.6,.6,.05]),
    ]
    if expected_particles == 125:
        box_rows=tags.get('BOX',[])
        require(len(box_rows)==6,'registered six boxes')
        for i,(position,half) in enumerate(registered_boxes):
            expected=[str(i),'0']+[str(bits32(v)) for v in position+[0,0,0,1]+[1,1,1]+half]
            require(box_rows[i]==expected,'registered box transform/geometry')
    wall_by_row = {}
    for row in wall_records:
        require(len(row) == 8 and int(row[0]) not in wall_by_row, 'wall arity/duplicate')
        wall_by_row[int(row[0])] = [decode(v) for v in row[1:]]
    reference = [0.0]*dimension
    lambdas, rhs = [], []
    for r, row in enumerate(rows):
        b, aa, multiplier = map(decode, row[1:4])
        require(multiplier >= 0 and aa > 0, 'negative multiplier/denominator')
        cs = coefficients[r]
        exact(serial_sum(a*a for _,a in cs), row[2], 'aa')
        if r < pair_count:
            i,j = pair_ids[r]
            require(kinds[r] == [str(r),'pair',str(i),str(j)], 'pair identity/order')
            expected_indices = [3*i+k for k in range(3)] + [3*j+k for k in range(3)]
            delta = [vectors['PREDICTED'][i][k]-vectors['PREDICTED'][j][k] for k in range(3)]
            length = math.sqrt(serial_sum(v*v for v in delta))
            normal = [v/length for v in delta] if length > 0 else [1.0,0.0,0.0]
            expected_a = [v/math.sqrt(masses[i]) for v in normal] + [-v/math.sqrt(masses[j]) for v in normal]
            close(2*radius-length, row[1], 'pair rhs')
        else:
            i,wall = divmod(r-pair_count, expected_walls)
            require(len(kinds[r]) == 5 and kinds[r][1:4] == ['wall',str(i),str(wall)], 'wall identity/order')
            surface_normal = wall_by_row.get(r)
            require(surface_normal is not None, 'missing wall metadata')
            surface,normal,signed = surface_normal[:3],surface_normal[3:6],surface_normal[6]
            close(serial_sum((vectors['PREDICTED'][i][k]-surface[k])*normal[k] for k in range(3)), str(bits(signed)), 'wall signed')
            require(math.isclose(serial_sum(v*v for v in normal),1.0,abs_tol=2e-12), 'wall unit normal')
            if expected_particles==125:
                position,half=registered_boxes[wall]
                pos=[decode(str(bits32(v)),32) for v in position]
                he=[decode(str(bits32(v)),32) for v in half]
                # Registered sealed cavity selects each body's cavity-facing face.
                axis=[1,1,0,0,2,2][wall]
                sign=[1,-1,1,-1,1,-1][wall]
                expected_normal=[0.0]*3; expected_normal[axis]=float(sign)
                require(normal==expected_normal and int(kinds[r][4])==axis,'registered wall normal/axis')
                face=decode(str(bits32(pos[axis]+sign*he[axis])),32)
                require(surface[axis]==face,'registered wall face')
                for k in range(3):
                    if k!=axis:
                        # f32 inverse/forward translation affects tangential coordinates.
                        local=decode(str(bits32(vectors['PREDICTED'][i][k]-pos[k])),32)
                        expected_surface=decode(str(bits32(local+pos[k])),32)
                        require(surface[k]==expected_surface,'registered surface tangent')
            close(radius-signed,row[1],'wall rhs')
            expected_indices = [3*i+k for k in range(3)]
            expected_a = [v/math.sqrt(masses[i]) for v in normal]
        require([i for i,_ in cs] == expected_indices, 'coefficient variable identity')
        require(all(math.isclose(a,b,rel_tol=2e-12,abs_tol=1e-14) for (_,a),b in zip(cs,expected_a)), 'massscaled coefficient')
        for i,a in cs:
            reference[i] += multiplier*a
        lambdas.append(multiplier)
        rhs.append(b)
    ys = indexed('Y', dimension, 5)
    incremental, inverse = [], []
    for i,row in enumerate(ys):
        inc,ref,inv,disp = map(decode,row[1:])
        require(inv > 0 and math.isclose(inv,1/math.sqrt(masses[i//3]),rel_tol=2e-12), 'inverse mass')
        exact(reference[i],row[2],'A-transpose-lambda')
        exact(ref*inv,row[4],'physical displacement')
        incremental.append(inc)
        inverse.append(inv)
    residuals = []
    for r,row in enumerate(rows):
        value = rhs[r] - serial_sum(a*reference[i] for i,a in coefficients[r])
        exact(value,row[4],'physical residual')
        residuals.append(value)
    maximum = max(0.0,max(residuals))
    require(maximum > 1e-7, 'capture is not a budget residual')
    order = sorted(range(count),key=lambda i:(-residuals[i],i))
    top = tags.get('TOP',[])
    require(len(top) == min(16,count), 'top count')
    for row,r in zip(top,order):
        require(len(row)==2 and int(row[0])==r,'top order')
        exact(residuals[r],row[1],'top residual')
    drifts=[]
    for i,row in enumerate(indexed('DRIFT',n,2)):
        drift=math.sqrt(serial_sum(((incremental[k]-reference[k])*inverse[k])**2 for k in range(3*i,3*i+3)))
        close(drift,row[1],'drift')
        drifts.append(drift)
    require(max(drifts)<=1e-7,'drift exceeds registered tolerance')
    dual=serial_sum(b*l for b,l in zip(rhs,lambdas))-.5*serial_sum(v*v for v in reference)
    summary=one('SUMMARY',9)
    require(int(summary[0])==order[0] and summary[8]=='true','summary row/finite')
    close(maximum,summary[1],'summary residual')
    require(int(summary[2])==sum(v>1e-7 for v in residuals[:pair_count]),'pair violating count')
    require(int(summary[3])==sum(v>1e-7 for v in residuals[pair_count:]),'wall violating count')
    for actual,encoded,label in [(min(lambdas),summary[4],'min lambda'),(max(lambdas),summary[5],'max lambda'),(max(drifts),summary[6],'max drift'),(dual,summary[7],'dual')]:
        close(actual,encoded,label)
    history=tags.get('SWEEP',[])
    require(len(history)==128,'history count')
    for i,row in enumerate(history):
        require(len(row)==4 and int(row[0])==i+1,'sweep order')
        maximum_i,dual_i,norm_i=map(decode,row[1:])
        require(maximum_i>=0 and norm_i>=0,'sweep scalar sign')
    last=history[-1]
    close(max(0.0,max(rhs[r]-serial_sum(a*incremental[i] for i,a in cs) for r,cs in enumerate(coefficients))),last[1],'last sweep residual')
    close(serial_sum(b*l for b,l in zip(rhs,lambdas))-.5*serial_sum(v*v for v in incremental),last[2],'last sweep dual')
    close(math.sqrt(serial_sum(l*l for l in lambdas)),last[3],'last sweep lambda norm')
    public=one('PUBLIC_FINAL',5)
    require(int(public[0])==step and public[1]==context[1] and public[2]==context[2],'public step context')
    final=vectors['FINAL_POSITION']
    distance=min(math.sqrt(serial_sum((p[k]-q[k])**2 for k in range(3))) for i,p in enumerate(final) for q in final[i+1:])
    clearance=min(min(.5-abs(p[0]),.5-abs(p[2]),p[1],1-p[1])-radius for p in final)
    close(distance,public[3],'final minimum pair')
    close(clearance,public[4],'final clearance')
    for tag in ['BODY','BOX']:
        require(len(tags.get(tag,[]))==expected_walls,'boundary count '+tag)
    for i,row in enumerate(tags.get('BODY',[])):
        require(row==[str(i),'Static','false','1'],'supported boundary identity')
    for i,row in enumerate(tags.get('BOX',[])):
        require(len(row)==15 and row[:2]==[str(i),'0'],'box metadata identity')
        values=[decode(v,32) for v in row[2:]]
        require(values[7:10]==[1.0,1.0,1.0] and min(values[10:])>0,'box scale/geometry')
        require(math.isclose(serial_sum(v*v for v in values[3:7]),1.0,abs_tol=1e-6),'box rotation domain')
    if expected_particles==125:
        require(len(tags.get('DESC_F32',[]))==24,'descriptor scalars')
        for tag in ['DESC_MODES','DESC_FLAGS','DESC_LIMITS','DESC_DRAIN','DESC_INITIAL_POSITIONS','DESC_BITS','DESC']:
            one(tag)
        for name in ['center','half_extents','initial_velocity','emit_position','emit_velocity']:
            for value in indexed('DESC_VEC3_'+name,1,4)[0][1:]:
                decode(value,32)
        scalars=descriptor_scalars(tags['DESC_F32'])
        require(scalars.get('radius')==radius and scalars.get('spacing')==decode(str(bits32(.1)),32),'registered radius/spacing')
        require(one('DESC_MODES',4)[1:] == ['SweptMeshes','Legacy','GlobalHalfspaceExperimental'],'registered modes')
        require(one('DESC_FLAGS',4)==['true','false','false','true'],'registered feature flags')
        require(one('DESC_LIMITS',4)[:2]==['4','2000'],'registered iterations/capacity')
        require(one('DESC_DRAIN')==['none'] and one('DESC_INITIAL_POSITIONS')==['none'],'registered spawning/drain')
        descriptor=one('DESC_BITS',5)
        require(descriptor[:2]==[entry[3],str(bits32(scalars['spacing']))] and descriptor[3:]==['4','2000'],'descriptor bit consistency')
    def digest(selected):
        return hashlib.sha256(('\n'.join(line for line in lines if line.split('\t')[0] in selected)+'\n').encode()).hexdigest()
    return {'integrity':'passed','not_a_feasibility_or_precision_result':True,'particles':n,'rows':count,'sweeps':128,'first_rejection_step':step,'max_residual_m':maximum,'max_residual_row':order[0],'max_residual_kind':kinds[order[0]] , 'dual_objective':dual,'lambda_min':min(lambdas),'lambda_max':max(lambdas),'lambda_l2':math.sqrt(serial_sum(l*l for l in lambdas)),'dual_updates':updates,'dual_decreases':decreases,'reference_bits_exact':True,'rollback_all_bits_exact':True,'entry_calls':calls,'pair_violations':int(summary[2]),'wall_violations':int(summary[3]),'max_drift_m':max(drifts),'snapshot_sha256':hashlib.sha256(data).hexdigest(),'input_sha256':digest({'ENTRY','PUBLIC_ENTRY_CONTEXT','PREDICTED','POSITION','VELOCITY','MASS'}),'ordered_graph_sha256':digest({'KIND','WALL','ROW','COEFF'}),'reference_source_sha256':identity[1],'instrumented_source_sha256':identity[2],'comparison_policy':{'reference_aa_residual_displacement':'exact f64 bits in serial order','summaries':'abs 1e-12 / rel 2e-12','physical_certificate_m':1e-7,'history_limitation':'Only final sweep independently reconstructible; prior scalar entries checked finite/structural, not replayed.'}}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshot',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    try:
        result=verify(args.snapshot.read_bytes())
    except (IntegrityError,ValueError,KeyError,IndexError,OverflowError) as error:
        parser.exit(1,'snapshot integrity FAILED: '+str(error)+'\n')
    args.output.write_text(json.dumps(result,indent=2,allow_nan=False)+'\n')
    print(json.dumps(result,sort_keys=True,allow_nan=False))


if __name__=='__main__':
    main()
