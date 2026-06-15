//! PostGIS bridge: routes SQLite scalar calls into postgis-wasm.
//!
//! Geometry crosses the boundary as BLOB containing WKB. Each
//! call reconstitutes the postgis-wasm `geometry` resource from
//! WKB at the boundary, performs the op, and materializes a
//! WKB BLOB on the way back when the result is itself a
//! geometry.
//!
//! The dispatch surface is large (~110 functions) but mostly
//! pattern-matched: macros below collapse the boilerplate so
//! adding the next batch of postgis-wasm exports is one line of
//! manifest + one line of dispatch each.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "bridge",
        generate_all,
    });
}

use bindings::exports::sqlite::extension::aggregate_function::Guest as AggregateGuest;
use bindings::exports::sqlite::extension::metadata::{
    AggregateFunctionSpec, Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
};
use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

use bindings::postgis::wasm::postgis_accessors as pg_acc;
use bindings::postgis::wasm::postgis_aggregates as pg_agg;
use bindings::postgis::wasm::postgis_clustering as pg_cluster;
use bindings::postgis::wasm::postgis_spatial_index as pg_strtree;
use bindings::postgis::wasm::postgis_constructors as pg_ctor;
use bindings::postgis::wasm::postgis_measurements as pg_meas;
use bindings::postgis::wasm::postgis_output as pg_out;
use bindings::postgis::wasm::postgis_predicates as pg_pred;
use bindings::postgis::wasm::postgis_processing as pg_proc;
use bindings::postgis::wasm::postgis_transformations as pg_xform;
use bindings::postgis::wasm::postgis_linear_ref as pg_lin;
use bindings::postgis::wasm::postgis_three_d as pg_threed;
use bindings::postgis::wasm::postgis_types::{Geography, Geometry};
use bindings::postgis::wasm::postgis_geodetic as pg_geog;
use bindings::postgis::wasm::postgis_sfcgal as pg_sfcgal;
use bindings::postgis::wasm::postgis_raster_accessors as pg_rast_acc;
use bindings::postgis::wasm::postgis_raster_constructors as pg_rast_ctor;
use bindings::postgis::wasm::postgis_raster_stats as pg_rast_stats;
use bindings::postgis::wasm::postgis_topology_output as pg_topo_out;
use bindings::postgis::wasm::postgis_topology_types::Topology;
use bindings::postgis::wasm::postgis_raster_pixels as pg_rast_px;
use bindings::postgis::wasm::postgis_raster_output as pg_rast_out;
use bindings::postgis::wasm::postgis_raster_predicates as pg_rast_pred;
use bindings::postgis::wasm::postgis_raster_processing as pg_rast_proc;
use bindings::postgis::wasm::postgis_raster_vector as pg_rast_vec;
use bindings::postgis::wasm::postgis_raster_types::Raster;
use bindings::sfcgal::component::geometry as sf_geom;
use bindings::sfcgal::component::io as sf_io;

use core::cell::RefCell;
use std::collections::HashMap;

// Function ids. Append-only. Ranges by category to leave space:
//   1..50    constructors
//   50..100  accessors
//   100..150 measurements
//   150..200 predicates
//   200..250 processing
//   250..300 output

// Constructors
const FID_ST_MAKEPOINT: u64 = 1;
const FID_ST_MAKEPOINT_Z: u64 = 2;
const FID_ST_MAKEPOINT_M: u64 = 3;
const FID_ST_MAKEPOINT_ZM: u64 = 4;
const FID_ST_POINT: u64 = 5;
const FID_ST_POINT_Z: u64 = 6;
const FID_ST_POINT_M: u64 = 7;
const FID_ST_POINT_ZM: u64 = 8;
const FID_ST_MAKE_ENVELOPE: u64 = 9;
const FID_ST_MAKE_ENVELOPE_SRID: u64 = 10;
const FID_ST_GEOMFROMTEXT: u64 = 11;
const FID_ST_GEOMFROMTEXT_SRID: u64 = 12;
const FID_ST_GEOMFROMEWKT: u64 = 13;
const FID_ST_POINTFROMTEXT: u64 = 14;
const FID_ST_GEOMFROMWKB: u64 = 15;
const FID_ST_GEOMFROMGEOJSON: u64 = 16;
const FID_ST_MAKE_LINE_TWO: u64 = 17;

// Accessors
const FID_ST_X: u64 = 50;
const FID_ST_Y: u64 = 51;
const FID_ST_XMIN: u64 = 52;
const FID_ST_XMAX: u64 = 53;
const FID_ST_YMIN: u64 = 54;
const FID_ST_YMAX: u64 = 55;
const FID_ST_SRID: u64 = 56;
const FID_ST_GEOMETRY_TYPE: u64 = 57;
const FID_ST_IS_EMPTY: u64 = 58;
const FID_ST_IS_VALID: u64 = 59;
const FID_ST_IS_SIMPLE: u64 = 60;
const FID_ST_IS_CLOSED: u64 = 61;
const FID_ST_IS_RING: u64 = 62;
const FID_ST_NUM_POINTS: u64 = 63;
const FID_ST_NUM_GEOMETRIES: u64 = 64;
const FID_ST_NUM_INTERIOR_RINGS: u64 = 65;
const FID_ST_NPOINTS: u64 = 66;
const FID_ST_EXTERIOR_RING: u64 = 67;
const FID_ST_INTERIOR_RING_N: u64 = 68;
const FID_ST_POINT_N: u64 = 69;
const FID_ST_GEOMETRY_N: u64 = 70;
const FID_ST_START_POINT: u64 = 71;
const FID_ST_END_POINT: u64 = 72;
const FID_ST_BOUNDARY: u64 = 73;
const FID_ST_ENVELOPE: u64 = 74;
const FID_ST_SET_SRID: u64 = 75;

// Measurements
const FID_ST_AREA: u64 = 100;
const FID_ST_LENGTH: u64 = 101;
const FID_ST_PERIMETER: u64 = 102;
const FID_ST_LENGTH_TWOD: u64 = 103;
const FID_ST_LENGTH_THREED: u64 = 104;
const FID_ST_PERIMETER_THREED: u64 = 105;
const FID_ST_DISTANCE: u64 = 106;
const FID_ST_DISTANCE_THREED: u64 = 107;
const FID_ST_MAX_DISTANCE: u64 = 108;
const FID_ST_MAX_DISTANCE_THREED: u64 = 109;
const FID_ST_HAUSDORFF_DISTANCE: u64 = 110;
const FID_ST_FRECHET_DISTANCE: u64 = 111;

// Predicates
const FID_ST_INTERSECTS: u64 = 150;
const FID_ST_CONTAINS: u64 = 151;
const FID_ST_WITHIN: u64 = 152;
const FID_ST_EQUALS: u64 = 153;
const FID_ST_DISJOINT: u64 = 154;
const FID_ST_OVERLAPS: u64 = 155;
const FID_ST_TOUCHES: u64 = 156;
const FID_ST_CROSSES: u64 = 157;
const FID_ST_COVERED_BY: u64 = 158;
const FID_ST_COVERS: u64 = 159;
const FID_ST_CONTAINS_PROPERLY: u64 = 160;
const FID_ST_3D_INTERSECTS: u64 = 161;
const FID_ST_3D_DISJOINT: u64 = 162;

// Processing
const FID_ST_BUFFER: u64 = 200;
const FID_ST_INTERSECTION: u64 = 201;
const FID_ST_UNION: u64 = 202;
const FID_ST_DIFFERENCE: u64 = 203;
const FID_ST_SYM_DIFFERENCE: u64 = 204;
const FID_ST_UNARY_UNION: u64 = 205;
const FID_ST_SIMPLIFY: u64 = 206;
const FID_ST_SIMPLIFY_PT: u64 = 207;
const FID_ST_SIMPLIFY_VW: u64 = 208;
const FID_ST_CONVEX_HULL: u64 = 209;
const FID_ST_CONCAVE_HULL: u64 = 210;
const FID_ST_CENTROID: u64 = 211;
const FID_ST_POINT_ON_SURFACE: u64 = 212;
const FID_ST_ORIENTED_ENVELOPE: u64 = 213;
const FID_ST_MIN_BOUNDING_CIRCLE: u64 = 214;
const FID_ST_LINE_MERGE: u64 = 215;
const FID_ST_MAKE_VALID: u64 = 216;
const FID_ST_REVERSE: u64 = 217;
const FID_ST_FLIP_COORDINATES: u64 = 218;
const FID_ST_FORCE_2D: u64 = 219;
const FID_ST_FORCE_3D: u64 = 220;
const FID_ST_MULTI: u64 = 221;
const FID_ST_COLLECTION_HOMOGENIZE: u64 = 222;

// Output
const FID_ST_ASTEXT: u64 = 250;
const FID_ST_ASBINARY: u64 = 251;
const FID_ST_AS_EWKT: u64 = 252;
const FID_ST_AS_EWKB: u64 = 253;
const FID_ST_AS_HEXEWKB: u64 = 254;
const FID_ST_AS_GEOJSON: u64 = 255;
const FID_ST_AS_SVG: u64 = 256;
const FID_ST_AS_KML: u64 = 257;
const FID_ST_AS_GML: u64 = 258;
const FID_ST_AS_X3D: u64 = 259;
const FID_ST_SUMMARY: u64 = 260;
const FID_ST_GEOHASH: u64 = 261;

// ── v2 batch IDs ──
// More accessors
const FID_ST_Z: u64 = 280;
const FID_ST_M: u64 = 281;
const FID_ST_ZMIN: u64 = 282;
const FID_ST_ZMAX: u64 = 283;
const FID_ST_MMIN: u64 = 284;
const FID_ST_MMAX: u64 = 285;
const FID_ST_NRINGS: u64 = 286;
const FID_ST_DIMENSION: u64 = 287;
const FID_ST_COORD_DIM: u64 = 288;
const FID_ST_NDIMS: u64 = 289;
const FID_ST_ZMFLAG: u64 = 290;
const FID_ST_MEM_SIZE: u64 = 291;
const FID_ST_IS_COLLECTION: u64 = 292;
const FID_ST_HAS_ARC_ACC: u64 = 293;
const FID_ST_POINTS: u64 = 294;
const FID_ST_BOUNDING_DIAGONAL: u64 = 295;
const FID_ST_EXPAND: u64 = 296;
const FID_ST_COLLECTION_EXTRACT: u64 = 297;

// More measurements
const FID_ST_CLOSEST_POINT: u64 = 320;
const FID_ST_CLOSEST_POINT_3D: u64 = 321;
const FID_ST_SHORTEST_LINE: u64 = 322;
const FID_ST_SHORTEST_LINE_3D: u64 = 323;
const FID_ST_LONGEST_LINE: u64 = 324;
const FID_ST_LONGEST_LINE_3D: u64 = 325;
const FID_ST_AZIMUTH: u64 = 326;
const FID_ST_ANGLE: u64 = 327;
const FID_ST_MIN_CLEARANCE: u64 = 328;
const FID_ST_MIN_CLEARANCE_LINE: u64 = 329;
const FID_ST_DISTANCE_CPA: u64 = 330;
const FID_ST_DISTANCE_SPHEROID: u64 = 331;
const FID_ST_LENGTH_SPHEROID: u64 = 332;

// More predicates
const FID_ST_DWITHIN: u64 = 360;
const FID_ST_DWITHIN_3D: u64 = 361;
const FID_ST_DFULLY_WITHIN: u64 = 362;
const FID_ST_EQUALS_EXACT: u64 = 363;
const FID_ST_RELATE: u64 = 364;
const FID_ST_RELATE_MATCH: u64 = 365;
const FID_ST_ORDERING_EQUALS: u64 = 366;
const FID_ST_HAS_Z: u64 = 367;
const FID_ST_HAS_M: u64 = 368;
const FID_ST_IS_POLYGON_CW: u64 = 369;
const FID_ST_IS_POLYGON_CCW: u64 = 370;
const FID_ST_IS_VALID_TRAJECTORY: u64 = 371;
const FID_ST_POINT_INSIDE_CIRCLE: u64 = 372;
const FID_ST_CONTAINS_3D: u64 = 373;
const FID_ST_CPA_WITHIN: u64 = 374;

// More processing
const FID_ST_CHAIKIN_SMOOTHING: u64 = 400;
const FID_ST_FORCE_RHR: u64 = 401;
const FID_ST_NORMALIZE: u64 = 402;
const FID_ST_REMOVE_REPEATED_POINTS: u64 = 403;
const FID_ST_SNAP_TO_GRID: u64 = 404;
const FID_ST_SNAP: u64 = 405;
const FID_ST_REDUCE_PRECISION: u64 = 406;
const FID_ST_LINE_MERGE_DIRECTED: u64 = 407;
const FID_ST_OFFSET_CURVE: u64 = 408;
const FID_ST_SHARED_PATHS: u64 = 409;
const FID_ST_VORONOI_POLYGONS: u64 = 410;
const FID_ST_VORONOI_LINES: u64 = 411;
const FID_ST_DELAUNAY_TRIANGLES: u64 = 412;
const FID_ST_CONSTRAINED_DELAUNAY: u64 = 413;
const FID_ST_GENERATE_POINTS: u64 = 414;
const FID_ST_SEGMENTIZE: u64 = 415;
const FID_ST_FORCE_POLYGON_CW: u64 = 416;
const FID_ST_FORCE_POLYGON_CCW: u64 = 417;
const FID_ST_SPLIT: u64 = 418;
const FID_ST_NODE: u64 = 419;
const FID_ST_POLYGONIZE: u64 = 420;
const FID_ST_BUILD_AREA: u64 = 421;
const FID_ST_CLIP_BY_BOX2D: u64 = 422;
const FID_ST_GEOMETRIC_MEDIAN: u64 = 423;
const FID_ST_MIN_BOUNDING_RADIUS: u64 = 424;
const FID_ST_MEM_UNION: u64 = 425;
const FID_ST_MAX_INSCRIBED_CIRCLE: u64 = 426;
const FID_ST_NUM_CURVES: u64 = 427;
const FID_ST_LINE_TO_CURVE: u64 = 428;
const FID_ST_FORCE_CURVE: u64 = 429;
const FID_ST_TRIANGULATE_POLYGON: u64 = 430;

// More output
const FID_ST_AS_TWKB: u64 = 460;
const FID_ST_AS_ENCODED_POLYLINE: u64 = 461;
const FID_ST_AS_LAT_LON_TEXT: u64 = 462;

// Transformations
const FID_ST_TRANSLATE: u64 = 500;
const FID_ST_SCALE: u64 = 501;
const FID_ST_TRANSSCALE: u64 = 502;
const FID_ST_ROTATE: u64 = 503;
const FID_ST_ROTATE_X: u64 = 504;
const FID_ST_ROTATE_Y: u64 = 505;
const FID_ST_ROTATE_Z: u64 = 506;
const FID_ST_AFFINE: u64 = 507;
const FID_ST_SWAP_ORDINATES: u64 = 508;
const FID_ST_FORCE_3DZ: u64 = 509;
const FID_ST_FORCE_3DM: u64 = 510;
const FID_ST_FORCE_4D: u64 = 511;
const FID_ST_FORCE_COLLECTION: u64 = 512;
const FID_ST_SHIFT_LONGITUDE: u64 = 513;
const FID_ST_WRAP_X: u64 = 514;
const FID_ST_QUANTIZE_COORDS: u64 = 515;
const FID_ST_FORCE_SFS: u64 = 516;
const FID_ST_TRANSFORM: u64 = 517;
const FID_ST_TRANSFORM_PIPELINE: u64 = 518;
const FID_ST_INV_TRANSFORM_PIPELINE: u64 = 519;

// Linear-ref
const FID_ST_LINE_INTERPOLATE_POINT: u64 = 540;
const FID_ST_LINE_INTERPOLATE_POINTS: u64 = 541;
const FID_ST_LINE_LOCATE_POINT: u64 = 542;
const FID_ST_LINE_SUBSTRING: u64 = 543;
const FID_ST_ADD_POINT: u64 = 544;
const FID_ST_SET_POINT: u64 = 545;
const FID_ST_REMOVE_POINT: u64 = 546;
const FID_ST_ADD_MEASURE: u64 = 547;
const FID_ST_LOCATE_ALONG: u64 = 548;
const FID_ST_LOCATE_BETWEEN: u64 = 549;
const FID_ST_LINE_EXTEND: u64 = 550;
const FID_ST_LINE_CROSSING_DIRECTION: u64 = 551;
const FID_ST_LINE_INTERPOLATE_POINT_3D: u64 = 552;
const FID_ST_LOCATE_BETWEEN_ELEVATIONS: u64 = 553;

// Three-d
const FID_ST_REVERSE_3D: u64 = 580;
const FID_ST_CENTROID_3D: u64 = 581;
const FID_ST_ENVELOPE_3D: u64 = 582;
const FID_ST_BOUNDARY_3D: u64 = 583;

// More constructors (parsers)
const FID_ST_LINE_FROM_TEXT: u64 = 600;
const FID_ST_POLYGON_FROM_TEXT: u64 = 601;
const FID_ST_MPOINT_FROM_TEXT: u64 = 602;
const FID_ST_MLINE_FROM_TEXT: u64 = 603;
const FID_ST_MPOLY_FROM_TEXT: u64 = 604;
const FID_ST_GEOMCOLL_FROM_TEXT: u64 = 605;
const FID_ST_GEOM_FROM_EWKB: u64 = 606;
const FID_ST_GEOM_FROM_HEXEWKB: u64 = 607;
const FID_ST_GEOM_FROM_GEOHASH: u64 = 608;
const FID_ST_POINT_FROM_GEOHASH: u64 = 609;
const FID_ST_GEOM_FROM_KML: u64 = 610;
const FID_ST_GEOM_FROM_GML: u64 = 611;
const FID_ST_GEOM_FROM_TWKB: u64 = 612;
const FID_ST_LINE_FROM_ENCODED_POLY: u64 = 613;

// Geodetic (geometry-typed helpers)
const FID_ST_DISTANCE_SPHERE: u64 = 640;
const FID_ST_PROJECT: u64 = 641;

// Geography-typed: input/output as BLOB (geography WKB).
const FID_ST_GEOGFROMTEXT: u64 = 700;
const FID_ST_GEOGFROMWKB: u64 = 701;
const FID_ST_GEOG_POINT: u64 = 702;
const FID_ST_GEOG_ASTEXT: u64 = 703;
const FID_ST_GEOG_DISTANCE: u64 = 704;
const FID_ST_GEOG_LENGTH: u64 = 705;
const FID_ST_GEOG_AREA: u64 = 706;
const FID_ST_GEOG_PERIMETER: u64 = 707;
const FID_ST_GEOG_DWITHIN: u64 = 708;
const FID_ST_GEOG_AZIMUTH: u64 = 709;
const FID_ST_GEOG_PROJECT: u64 = 710;
const FID_ST_GEOG_SEGMENTIZE: u64 = 711;
const FID_ST_GEOG_COVERS: u64 = 712;
const FID_ST_GEOG_COVERED_BY: u64 = 713;
const FID_ST_GEOG_INTERSECTS: u64 = 714;
const FID_ST_GEOG_BUFFER: u64 = 715;
const FID_ST_GEOG_BUFFER_SEGS: u64 = 716;
const FID_ST_GEOG_CENTROID: u64 = 717;
const FID_ST_GEOG_INTERSECTION: u64 = 718;
const FID_ST_GEOG_UNION: u64 = 719;
const FID_ST_GEOG_DIFFERENCE: u64 = 720;
const FID_ST_GEOG_SYM_DIFFERENCE: u64 = 721;
const FID_ST_GEOG_EXPAND: u64 = 722;
const FID_ST_GEOG_CLOSEST_POINT: u64 = 723;
const FID_ST_GEOG_NPOINTS: u64 = 724;
const FID_ST_GEOG_SUMMARY: u64 = 725;
const FID_ST_GEOG_GEOMETRY_TYPE: u64 = 726;
const FID_ST_GEOG_IS_EMPTY: u64 = 727;
const FID_ST_GEOG_IS_SIMPLE: u64 = 728;
const FID_ST_GEOG_IS_CLOSED: u64 = 729;
const FID_ST_GEOG_CONVEX_HULL: u64 = 730;
const FID_ST_GEOG_TO_GEOMETRY: u64 = 731;
const FID_ST_GEOMETRY_TO_GEOG: u64 = 732;

// SFCGAL (postgis-sfcgal — WKB-in / WKB-out, no resource handles).
const FID_ST_CONVEX_HULL_3D: u64 = 770;
const FID_ST_UNION_3D: u64 = 771;
const FID_ST_INTERSECTION_3D: u64 = 772;
const FID_ST_DIFFERENCE_3D: u64 = 773;
const FID_ST_TESSELATE: u64 = 774;
const FID_ST_STRAIGHT_SKELETON: u64 = 775;
const FID_ST_APPROX_MEDIAL_AXIS: u64 = 776;
const FID_ST_EXTRUDE: u64 = 777;
const FID_ST_MINKOWSKI_SUM: u64 = 778;
const FID_ST_VOLUME: u64 = 779;
const FID_ST_AREA_3D: u64 = 780;
const FID_ST_DISTANCE_3D_SFCGAL: u64 = 781;
const FID_ST_TRANSLATE_3D: u64 = 782;
const FID_ST_SCALE_3D: u64 = 783;
const FID_ST_ROTATE_3D: u64 = 784;

// Direct sfcgal-wasm (geometry-handle keyed; results materialized
// through write-wkb on the way back). Prefix `st_sfc_*` so they
// don't collide with the postgis-sfcgal names.
const FID_SFC_AS_STL: u64 = 800;
const FID_SFC_AS_STL_BINARY: u64 = 801;
const FID_SFC_AS_OBJ: u64 = 802;
const FID_SFC_AS_VTK: u64 = 803;
const FID_SFC_ALPHA_SHAPE: u64 = 804;
const FID_SFC_OPTIMAL_ALPHA_SHAPE: u64 = 805;
const FID_SFC_EXTRUDE_STRAIGHT: u64 = 806;
const FID_SFC_EXTRUDE_STRAIGHT_SKELETON: u64 = 807;
const FID_SFC_MAKE_VALID: u64 = 808;
const FID_SFC_IS_VALID: u64 = 809;
const FID_SFC_AREA: u64 = 810;
const FID_SFC_VOLUME: u64 = 811;
const FID_SFC_LENGTH: u64 = 812;
const FID_SFC_DISTANCE: u64 = 813;
const FID_SFC_VERSION: u64 = 814;
const FID_SFC_TRIANGLE: u64 = 815;
const FID_SFC_TESSELLATE: u64 = 816;
const FID_SFC_CONVEX_HULL: u64 = 817;
const FID_SFC_DIFFERENCE: u64 = 818;
const FID_SFC_INTERSECTION: u64 = 819;
const FID_SFC_UNION: u64 = 820;

// Raster (postgis-raster-*). Raster crosses as BLOB containing
// raster bytes — same pattern as geometry.
const FID_RST_WIDTH: u64 = 900;
const FID_RST_HEIGHT: u64 = 901;
const FID_RST_NUM_BANDS: u64 = 902;
const FID_RST_UPPER_LEFT_X: u64 = 903;
const FID_RST_UPPER_LEFT_Y: u64 = 904;
const FID_RST_SCALE_X: u64 = 905;
const FID_RST_SCALE_Y: u64 = 906;
const FID_RST_SKEW_X: u64 = 907;
const FID_RST_SKEW_Y: u64 = 908;
const FID_RST_SRID: u64 = 909;
const FID_RST_HAS_NO_BAND: u64 = 910;
const FID_RST_VALUE: u64 = 911;
const FID_RST_NEAREST_VALUE: u64 = 912;
const FID_RST_PIXEL_AS_POINT: u64 = 913;
const FID_RST_PIXEL_AS_POLYGON: u64 = 914;
const FID_RST_PIXEL_AS_CENTROID: u64 = 915;
const FID_RST_RASTER_TO_WORLD_COORD_X: u64 = 916;
const FID_RST_RASTER_TO_WORLD_COORD_Y: u64 = 917;
const FID_RST_AS_PNG: u64 = 918;
const FID_RST_AS_TIFF: u64 = 919;
const FID_RST_R_INTERSECTS: u64 = 920;
const FID_RST_R_CONTAINS: u64 = 921;
const FID_RST_R_WITHIN: u64 = 922;
const FID_RST_R_COVERS: u64 = 923;
const FID_RST_R_OVERLAPS: u64 = 924;
const FID_RST_R_INTERSECTS_GEOM: u64 = 925;
const FID_RST_R_CONTAINS_GEOM: u64 = 926;
const FID_RST_POLYGON_FROM_RAST: u64 = 927;
const FID_RST_CONVEX_HULL: u64 = 928;
const FID_RST_SLOPE: u64 = 929;
const FID_RST_ASPECT: u64 = 930;
const FID_RST_ROUGHNESS: u64 = 931;
const FID_RST_TRI: u64 = 932;
const FID_RST_TPI: u64 = 933;

// Raster v2 batch
const FID_RST_MAKE_EMPTY: u64 = 940;
const FID_RST_ADD_BAND: u64 = 941;
const FID_RST_SET_VALUE: u64 = 942;
const FID_RST_SUMMARY_COUNT: u64 = 943;
const FID_RST_SUMMARY_SUM: u64 = 944;
const FID_RST_SUMMARY_MEAN: u64 = 945;
const FID_RST_SUMMARY_STDDEV: u64 = 946;
const FID_RST_SUMMARY_MIN: u64 = 947;
const FID_RST_SUMMARY_MAX: u64 = 948;
const FID_RST_QUANTILE: u64 = 949;
const FID_RST_WORLD_TO_RAST_X: u64 = 950;
const FID_RST_WORLD_TO_RAST_Y: u64 = 951;
const FID_RST_HILL_SHADE: u64 = 952;
const FID_RST_RESIZE: u64 = 953;
const FID_RST_RESCALE: u64 = 954;
const FID_RST_BAND_PIXEL_TYPE: u64 = 955;
const FID_RST_BAND_NODATA: u64 = 956;

// Topology
const FID_TOPO_NAME: u64 = 970;
const FID_TOPO_SRID: u64 = 971;
const FID_TOPO_PRECISION: u64 = 972;
const FID_TOPO_NODE_COUNT: u64 = 973;
const FID_TOPO_EDGE_COUNT: u64 = 974;
const FID_TOPO_FACE_COUNT: u64 = 975;
const FID_TOPO_AS_TOPOJSON: u64 = 976;

// Spatial-index (STRtree). Handles cross as INTEGER. State
// lives in the cached stateful Store  trees survive across
// scalar dispatches via the host's Store cache.
const FID_STRTREE_CREATE: u64 = 980;
const FID_STRTREE_INSERT: u64 = 981;
const FID_STRTREE_BUILD: u64 = 982;
const FID_STRTREE_QUERY: u64 = 983;
const FID_STRTREE_NEAREST: u64 = 984;
const FID_STRTREE_KNN: u64 = 985;
const FID_STRTREE_WITHIN: u64 = 986;
const FID_STRTREE_DESTROY: u64 = 987;

// Aggregate function ids (separate namespace, but kept distinct
// from scalar ids for clarity).
const AGG_ST_UNION: u64 = 1000;
const AGG_ST_POLYGONIZE: u64 = 1001;
const AGG_ST_MAKELINE: u64 = 1002;
const AGG_ST_CLUSTER_INTERSECTING: u64 = 1003;
const AGG_ST_CLUSTER_WITHIN: u64 = 1004;
const AGG_ST_EXTENT_3D: u64 = 1005;
const AGG_ST_CLUSTER_DBSCAN: u64 = 1006;
const AGG_ST_CLUSTER_KMEANS: u64 = 1007;

/// Per-aggregation state: collected geometries (as WKB) plus
/// the trailing scalar args some aggregates latch from the
/// first row (cluster-within's distance, dbscan's eps +
/// min-points, kmeans' k).
#[derive(Default)]
struct AggState {
    wkbs: Vec<Vec<u8>>,
    distance: Option<f64>,
    eps: Option<f64>,
    min_points: Option<u32>,
    k: Option<u32>,
}

thread_local! {
    static AGGS: RefCell<HashMap<u64, AggState>> = RefCell::new(HashMap::new());
}

struct PostgisBridge;

impl MetadataGuest for PostgisBridge {
    fn describe() -> Manifest {
        let det = FunctionFlags::DETERMINISTIC;
        let s = |id, name: &str, num_args: i32| ScalarFunctionSpec {
            id,
            name: name.into(),
            num_args,
            func_flags: det,
        };
        Manifest {
            name: "postgis".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            scalar_functions: alloc::vec![
                // Constructors
                s(FID_ST_MAKEPOINT, "st_makepoint", 2),
                s(FID_ST_MAKEPOINT_Z, "st_makepointz", 3),
                s(FID_ST_MAKEPOINT_M, "st_makepointm", 3),
                s(FID_ST_MAKEPOINT_ZM, "st_makepointzm", 4),
                s(FID_ST_POINT, "st_point", 2),
                s(FID_ST_POINT_Z, "st_pointz", 3),
                s(FID_ST_POINT_M, "st_pointm", 3),
                s(FID_ST_POINT_ZM, "st_pointzm", 4),
                s(FID_ST_MAKE_ENVELOPE, "st_makeenvelope", 4),
                s(FID_ST_MAKE_ENVELOPE_SRID, "st_makeenvelope_srid", 5),
                s(FID_ST_GEOMFROMTEXT, "st_geomfromtext", 1),
                s(FID_ST_GEOMFROMTEXT_SRID, "st_geomfromtext_srid", 2),
                s(FID_ST_GEOMFROMEWKT, "st_geomfromewkt", 1),
                s(FID_ST_POINTFROMTEXT, "st_pointfromtext", 1),
                s(FID_ST_GEOMFROMWKB, "st_geomfromwkb", 1),
                s(FID_ST_GEOMFROMGEOJSON, "st_geomfromgeojson", 1),
                s(FID_ST_MAKE_LINE_TWO, "st_makeline", 2),
                // Accessors
                s(FID_ST_X, "st_x", 1),
                s(FID_ST_Y, "st_y", 1),
                s(FID_ST_XMIN, "st_xmin", 1),
                s(FID_ST_XMAX, "st_xmax", 1),
                s(FID_ST_YMIN, "st_ymin", 1),
                s(FID_ST_YMAX, "st_ymax", 1),
                s(FID_ST_SRID, "st_srid", 1),
                s(FID_ST_GEOMETRY_TYPE, "st_geometrytype", 1),
                s(FID_ST_IS_EMPTY, "st_isempty", 1),
                s(FID_ST_IS_VALID, "st_isvalid", 1),
                s(FID_ST_IS_SIMPLE, "st_issimple", 1),
                s(FID_ST_IS_CLOSED, "st_isclosed", 1),
                s(FID_ST_IS_RING, "st_isring", 1),
                s(FID_ST_NUM_POINTS, "st_numpoints", 1),
                s(FID_ST_NUM_GEOMETRIES, "st_numgeometries", 1),
                s(FID_ST_NUM_INTERIOR_RINGS, "st_numinteriorrings", 1),
                s(FID_ST_NPOINTS, "st_npoints", 1),
                s(FID_ST_EXTERIOR_RING, "st_exteriorring", 1),
                s(FID_ST_INTERIOR_RING_N, "st_interiorringn", 2),
                s(FID_ST_POINT_N, "st_pointn", 2),
                s(FID_ST_GEOMETRY_N, "st_geometryn", 2),
                s(FID_ST_START_POINT, "st_startpoint", 1),
                s(FID_ST_END_POINT, "st_endpoint", 1),
                s(FID_ST_BOUNDARY, "st_boundary", 1),
                s(FID_ST_ENVELOPE, "st_envelope", 1),
                s(FID_ST_SET_SRID, "st_setsrid", 2),
                // Measurements
                s(FID_ST_AREA, "st_area", 1),
                s(FID_ST_LENGTH, "st_length", 1),
                s(FID_ST_PERIMETER, "st_perimeter", 1),
                s(FID_ST_LENGTH_TWOD, "st_length2d", 1),
                s(FID_ST_LENGTH_THREED, "st_length3d", 1),
                s(FID_ST_PERIMETER_THREED, "st_perimeter3d", 1),
                s(FID_ST_DISTANCE, "st_distance", 2),
                s(FID_ST_DISTANCE_THREED, "st_distance3d", 2),
                s(FID_ST_MAX_DISTANCE, "st_maxdistance", 2),
                s(FID_ST_MAX_DISTANCE_THREED, "st_maxdistance3d", 2),
                s(FID_ST_HAUSDORFF_DISTANCE, "st_hausdorffdistance", 2),
                s(FID_ST_FRECHET_DISTANCE, "st_frechetdistance", 2),
                // Predicates
                s(FID_ST_INTERSECTS, "st_intersects", 2),
                s(FID_ST_CONTAINS, "st_contains", 2),
                s(FID_ST_WITHIN, "st_within", 2),
                s(FID_ST_EQUALS, "st_equals", 2),
                s(FID_ST_DISJOINT, "st_disjoint", 2),
                s(FID_ST_OVERLAPS, "st_overlaps", 2),
                s(FID_ST_TOUCHES, "st_touches", 2),
                s(FID_ST_CROSSES, "st_crosses", 2),
                s(FID_ST_COVERED_BY, "st_coveredby", 2),
                s(FID_ST_COVERS, "st_covers", 2),
                s(FID_ST_CONTAINS_PROPERLY, "st_containsproperly", 2),
                s(FID_ST_3D_INTERSECTS, "st_3dintersects", 2),
                s(FID_ST_3D_DISJOINT, "st_3ddisjoint", 2),
                // Processing
                s(FID_ST_BUFFER, "st_buffer", 2),
                s(FID_ST_INTERSECTION, "st_intersection", 2),
                s(FID_ST_UNION, "st_union", 2),
                s(FID_ST_DIFFERENCE, "st_difference", 2),
                s(FID_ST_SYM_DIFFERENCE, "st_symdifference", 2),
                s(FID_ST_UNARY_UNION, "st_unaryunion", 1),
                s(FID_ST_SIMPLIFY, "st_simplify", 2),
                s(FID_ST_SIMPLIFY_PT, "st_simplifypreservetopology", 2),
                s(FID_ST_SIMPLIFY_VW, "st_simplifyvw", 2),
                s(FID_ST_CONVEX_HULL, "st_convexhull", 1),
                s(FID_ST_CONCAVE_HULL, "st_concavehull", 2),
                s(FID_ST_CENTROID, "st_centroid", 1),
                s(FID_ST_POINT_ON_SURFACE, "st_pointonsurface", 1),
                s(FID_ST_ORIENTED_ENVELOPE, "st_orientedenvelope", 1),
                s(FID_ST_MIN_BOUNDING_CIRCLE, "st_minimumboundingcircle", 1),
                s(FID_ST_LINE_MERGE, "st_linemerge", 1),
                s(FID_ST_MAKE_VALID, "st_makevalid", 1),
                s(FID_ST_REVERSE, "st_reverse", 1),
                s(FID_ST_FLIP_COORDINATES, "st_flipcoordinates", 1),
                s(FID_ST_FORCE_2D, "st_force2d", 1),
                s(FID_ST_FORCE_3D, "st_force3d", 1),
                s(FID_ST_MULTI, "st_multi", 1),
                s(FID_ST_COLLECTION_HOMOGENIZE, "st_collectionhomogenize", 1),
                // Output
                s(FID_ST_ASTEXT, "st_astext", 1),
                s(FID_ST_ASBINARY, "st_asbinary", 1),
                s(FID_ST_AS_EWKT, "st_asewkt", 1),
                s(FID_ST_AS_EWKB, "st_asewkb", 1),
                s(FID_ST_AS_HEXEWKB, "st_ashexewkb", 1),
                s(FID_ST_AS_GEOJSON, "st_asgeojson", 1),
                s(FID_ST_AS_SVG, "st_assvg", 1),
                s(FID_ST_AS_KML, "st_askml", 1),
                s(FID_ST_AS_GML, "st_asgml", 1),
                s(FID_ST_AS_X3D, "st_asx3d", 1),
                s(FID_ST_SUMMARY, "st_summary", 1),
                s(FID_ST_GEOHASH, "st_geohash", 1),
                // v2 batch
                s(FID_ST_Z, "st_z", 1),
                s(FID_ST_M, "st_m", 1),
                s(FID_ST_ZMIN, "st_zmin", 1),
                s(FID_ST_ZMAX, "st_zmax", 1),
                s(FID_ST_MMIN, "st_mmin", 1),
                s(FID_ST_MMAX, "st_mmax", 1),
                s(FID_ST_NRINGS, "st_nrings", 1),
                s(FID_ST_DIMENSION, "st_dimension", 1),
                s(FID_ST_COORD_DIM, "st_coorddim", 1),
                s(FID_ST_NDIMS, "st_ndims", 1),
                s(FID_ST_ZMFLAG, "st_zmflag", 1),
                s(FID_ST_MEM_SIZE, "st_memsize", 1),
                s(FID_ST_IS_COLLECTION, "st_iscollection", 1),
                s(FID_ST_HAS_ARC_ACC, "st_hasarc", 1),
                s(FID_ST_POINTS, "st_points", 1),
                s(FID_ST_BOUNDING_DIAGONAL, "st_boundingdiagonal", 1),
                s(FID_ST_EXPAND, "st_expand", 2),
                s(FID_ST_COLLECTION_EXTRACT, "st_collectionextract", 2),
                s(FID_ST_CLOSEST_POINT, "st_closestpoint", 2),
                s(FID_ST_CLOSEST_POINT_3D, "st_3dclosestpoint", 2),
                s(FID_ST_SHORTEST_LINE, "st_shortestline", 2),
                s(FID_ST_SHORTEST_LINE_3D, "st_3dshortestline", 2),
                s(FID_ST_LONGEST_LINE, "st_longestline", 2),
                s(FID_ST_LONGEST_LINE_3D, "st_3dlongestline", 2),
                s(FID_ST_AZIMUTH, "st_azimuth", 2),
                s(FID_ST_ANGLE, "st_angle", 3),
                s(FID_ST_MIN_CLEARANCE, "st_minimumclearance", 1),
                s(FID_ST_MIN_CLEARANCE_LINE, "st_minimumclearanceline", 1),
                s(FID_ST_DISTANCE_CPA, "st_distancecpa", 2),
                s(FID_ST_DISTANCE_SPHEROID, "st_distancespheroid", 2),
                s(FID_ST_LENGTH_SPHEROID, "st_lengthspheroid", 1),
                s(FID_ST_DWITHIN, "st_dwithin", 3),
                s(FID_ST_DWITHIN_3D, "st_3ddwithin", 3),
                s(FID_ST_DFULLY_WITHIN, "st_dfullywithin", 3),
                s(FID_ST_EQUALS_EXACT, "st_equalsexact", 3),
                s(FID_ST_RELATE, "st_relate", 2),
                s(FID_ST_RELATE_MATCH, "st_relatematch", 3),
                s(FID_ST_ORDERING_EQUALS, "st_orderingequals", 2),
                s(FID_ST_HAS_Z, "st_hasz", 1),
                s(FID_ST_HAS_M, "st_hasm", 1),
                s(FID_ST_IS_POLYGON_CW, "st_ispolygoncw", 1),
                s(FID_ST_IS_POLYGON_CCW, "st_ispolygonccw", 1),
                s(FID_ST_IS_VALID_TRAJECTORY, "st_isvalidtrajectory", 1),
                s(FID_ST_POINT_INSIDE_CIRCLE, "st_pointinsidecircle", 4),
                s(FID_ST_CONTAINS_3D, "st_3dcontains", 2),
                s(FID_ST_CPA_WITHIN, "st_cpawithin", 3),
                s(FID_ST_CHAIKIN_SMOOTHING, "st_chaikinsmoothing", 2),
                s(FID_ST_FORCE_RHR, "st_forcerhr", 1),
                s(FID_ST_NORMALIZE, "st_normalize", 1),
                s(FID_ST_REMOVE_REPEATED_POINTS, "st_removerepeatedpoints", 1),
                s(FID_ST_SNAP_TO_GRID, "st_snaptogrid", 2),
                s(FID_ST_SNAP, "st_snap", 3),
                s(FID_ST_REDUCE_PRECISION, "st_reduceprecision", 2),
                s(FID_ST_LINE_MERGE_DIRECTED, "st_linemergedirected", 2),
                s(FID_ST_OFFSET_CURVE, "st_offsetcurve", 2),
                s(FID_ST_SHARED_PATHS, "st_sharedpaths", 2),
                s(FID_ST_VORONOI_POLYGONS, "st_voronoipolygons", 2),
                s(FID_ST_VORONOI_LINES, "st_voronoilines", 2),
                s(FID_ST_DELAUNAY_TRIANGLES, "st_delaunaytriangles", 2),
                s(FID_ST_CONSTRAINED_DELAUNAY, "st_constraineddelaunaytriangles", 1),
                s(FID_ST_GENERATE_POINTS, "st_generatepoints", 2),
                s(FID_ST_SEGMENTIZE, "st_segmentize", 2),
                s(FID_ST_FORCE_POLYGON_CW, "st_forcepolygoncw", 1),
                s(FID_ST_FORCE_POLYGON_CCW, "st_forcepolygonccw", 1),
                s(FID_ST_SPLIT, "st_split", 2),
                s(FID_ST_NODE, "st_node", 1),
                s(FID_ST_POLYGONIZE, "st_polygonize", 1),
                s(FID_ST_BUILD_AREA, "st_buildarea", 1),
                s(FID_ST_CLIP_BY_BOX2D, "st_clipbybox2d", 5),
                s(FID_ST_GEOMETRIC_MEDIAN, "st_geometricmedian", 1),
                s(FID_ST_MIN_BOUNDING_RADIUS, "st_minimumboundingradius", 1),
                s(FID_ST_MEM_UNION, "st_memunion", 1),
                s(FID_ST_MAX_INSCRIBED_CIRCLE, "st_maximuminscribedcircle", 1),
                s(FID_ST_NUM_CURVES, "st_numcurves", 1),
                s(FID_ST_LINE_TO_CURVE, "st_linetocurve", 1),
                s(FID_ST_FORCE_CURVE, "st_forcecurve", 1),
                s(FID_ST_TRIANGULATE_POLYGON, "st_triangulatepolygon", 1),
                s(FID_ST_AS_TWKB, "st_astwkb", 1),
                s(FID_ST_AS_ENCODED_POLYLINE, "st_asencodedpolyline", 1),
                s(FID_ST_AS_LAT_LON_TEXT, "st_aslatlontext", 1),
                s(FID_ST_TRANSLATE, "st_translate", 3),
                s(FID_ST_SCALE, "st_scale", 3),
                s(FID_ST_TRANSSCALE, "st_transscale", 5),
                s(FID_ST_ROTATE, "st_rotate", 2),
                s(FID_ST_ROTATE_X, "st_rotatex", 2),
                s(FID_ST_ROTATE_Y, "st_rotatey", 2),
                s(FID_ST_ROTATE_Z, "st_rotatez", 2),
                s(FID_ST_AFFINE, "st_affine", 7),
                s(FID_ST_SWAP_ORDINATES, "st_swapordinates", 2),
                s(FID_ST_FORCE_3DZ, "st_force3dz", 1),
                s(FID_ST_FORCE_3DM, "st_force3dm", 1),
                s(FID_ST_FORCE_4D, "st_force4d", 1),
                s(FID_ST_FORCE_COLLECTION, "st_forcecollection", 1),
                s(FID_ST_SHIFT_LONGITUDE, "st_shiftlongitude", 1),
                s(FID_ST_WRAP_X, "st_wrapx", 3),
                s(FID_ST_QUANTIZE_COORDS, "st_quantizecoordinates", 3),
                s(FID_ST_FORCE_SFS, "st_forcesfs", 1),
                s(FID_ST_TRANSFORM, "st_transform", 2),
                s(FID_ST_TRANSFORM_PIPELINE, "st_transformpipeline", 2),
                s(FID_ST_INV_TRANSFORM_PIPELINE, "st_inversetransformpipeline", 2),
                s(FID_ST_LINE_INTERPOLATE_POINT, "st_lineinterpolatepoint", 2),
                s(FID_ST_LINE_INTERPOLATE_POINTS, "st_lineinterpolatepoints", 3),
                s(FID_ST_LINE_LOCATE_POINT, "st_linelocatepoint", 2),
                s(FID_ST_LINE_SUBSTRING, "st_linesubstring", 3),
                s(FID_ST_ADD_POINT, "st_addpoint", 2),
                s(FID_ST_SET_POINT, "st_setpoint", 3),
                s(FID_ST_REMOVE_POINT, "st_removepoint", 2),
                s(FID_ST_ADD_MEASURE, "st_addmeasure", 3),
                s(FID_ST_LOCATE_ALONG, "st_locatealong", 2),
                s(FID_ST_LOCATE_BETWEEN, "st_locatebetween", 3),
                s(FID_ST_LINE_EXTEND, "st_lineextend", 3),
                s(FID_ST_LINE_CROSSING_DIRECTION, "st_linecrossingdirection", 2),
                s(FID_ST_LINE_INTERPOLATE_POINT_3D, "st_3dlineinterpolatepoint", 2),
                s(FID_ST_LOCATE_BETWEEN_ELEVATIONS, "st_locatebetweenelevations", 3),
                s(FID_ST_REVERSE_3D, "st_3dreverse", 1),
                s(FID_ST_CENTROID_3D, "st_3dcentroid", 1),
                s(FID_ST_ENVELOPE_3D, "st_3denvelope", 1),
                s(FID_ST_BOUNDARY_3D, "st_3dboundary", 1),
                s(FID_ST_LINE_FROM_TEXT, "st_linefromtext", 1),
                s(FID_ST_POLYGON_FROM_TEXT, "st_polygonfromtext", 1),
                s(FID_ST_MPOINT_FROM_TEXT, "st_mpointfromtext", 1),
                s(FID_ST_MLINE_FROM_TEXT, "st_mlinefromtext", 1),
                s(FID_ST_MPOLY_FROM_TEXT, "st_mpolyfromtext", 1),
                s(FID_ST_GEOMCOLL_FROM_TEXT, "st_geomcollfromtext", 1),
                s(FID_ST_GEOM_FROM_EWKB, "st_geomfromewkb", 1),
                s(FID_ST_GEOM_FROM_HEXEWKB, "st_geomfromhexewkb", 1),
                s(FID_ST_GEOM_FROM_GEOHASH, "st_geomfromgeohash", 1),
                s(FID_ST_POINT_FROM_GEOHASH, "st_pointfromgeohash", 1),
                s(FID_ST_GEOM_FROM_KML, "st_geomfromkml", 1),
                s(FID_ST_GEOM_FROM_GML, "st_geomfromgml", 1),
                s(FID_ST_GEOM_FROM_TWKB, "st_geomfromtwkb", 1),
                s(FID_ST_LINE_FROM_ENCODED_POLY, "st_linefromencodedpolyline", 1),
                // Geodetic (geometry-typed)
                s(FID_ST_DISTANCE_SPHERE, "st_distancesphere", 2),
                s(FID_ST_PROJECT, "st_project", 3),
                // Geography
                s(FID_ST_GEOGFROMTEXT, "st_geogfromtext", 1),
                s(FID_ST_GEOGFROMWKB, "st_geogfromwkb", 1),
                s(FID_ST_GEOG_POINT, "st_geogpoint", 2),
                s(FID_ST_GEOG_ASTEXT, "st_geog_astext", 1),
                s(FID_ST_GEOG_DISTANCE, "st_geog_distance", 2),
                s(FID_ST_GEOG_LENGTH, "st_geog_length", 1),
                s(FID_ST_GEOG_AREA, "st_geog_area", 1),
                s(FID_ST_GEOG_PERIMETER, "st_geog_perimeter", 1),
                s(FID_ST_GEOG_DWITHIN, "st_geog_dwithin", 3),
                s(FID_ST_GEOG_AZIMUTH, "st_geog_azimuth", 2),
                s(FID_ST_GEOG_PROJECT, "st_geog_project", 3),
                s(FID_ST_GEOG_SEGMENTIZE, "st_geog_segmentize", 2),
                s(FID_ST_GEOG_COVERS, "st_geog_covers", 2),
                s(FID_ST_GEOG_COVERED_BY, "st_geog_coveredby", 2),
                s(FID_ST_GEOG_INTERSECTS, "st_geog_intersects", 2),
                s(FID_ST_GEOG_BUFFER, "st_geog_buffer", 2),
                s(FID_ST_GEOG_BUFFER_SEGS, "st_geog_buffer_segs", 3),
                s(FID_ST_GEOG_CENTROID, "st_geog_centroid", 1),
                s(FID_ST_GEOG_INTERSECTION, "st_geog_intersection", 2),
                s(FID_ST_GEOG_UNION, "st_geog_union", 2),
                s(FID_ST_GEOG_DIFFERENCE, "st_geog_difference", 2),
                s(FID_ST_GEOG_SYM_DIFFERENCE, "st_geog_symdifference", 2),
                s(FID_ST_GEOG_EXPAND, "st_geog_expand", 2),
                s(FID_ST_GEOG_CLOSEST_POINT, "st_geog_closestpoint", 2),
                s(FID_ST_GEOG_NPOINTS, "st_geog_npoints", 1),
                s(FID_ST_GEOG_SUMMARY, "st_geog_summary", 1),
                s(FID_ST_GEOG_GEOMETRY_TYPE, "st_geog_geometrytype", 1),
                s(FID_ST_GEOG_IS_EMPTY, "st_geog_isempty", 1),
                s(FID_ST_GEOG_IS_SIMPLE, "st_geog_issimple", 1),
                s(FID_ST_GEOG_IS_CLOSED, "st_geog_isclosed", 1),
                s(FID_ST_GEOG_CONVEX_HULL, "st_geog_convexhull", 1),
                s(FID_ST_GEOG_TO_GEOMETRY, "st_geog_togeometry", 1),
                s(FID_ST_GEOMETRY_TO_GEOG, "st_togeography", 1),
                // SFCGAL (3D)
                s(FID_ST_CONVEX_HULL_3D, "st_3dconvexhull", 1),
                s(FID_ST_UNION_3D, "st_3dunion", 2),
                s(FID_ST_INTERSECTION_3D, "st_3dintersection", 2),
                s(FID_ST_DIFFERENCE_3D, "st_3ddifference", 2),
                s(FID_ST_TESSELATE, "st_tesselate", 1),
                s(FID_ST_STRAIGHT_SKELETON, "st_straightskeleton", 1),
                s(FID_ST_APPROX_MEDIAL_AXIS, "st_sfcgalapproximatemedialaxis", 1),
                s(FID_ST_EXTRUDE, "st_extrude", 4),
                s(FID_ST_MINKOWSKI_SUM, "st_minkowskisum", 2),
                s(FID_ST_VOLUME, "st_volume", 1),
                s(FID_ST_AREA_3D, "st_3darea", 1),
                s(FID_ST_DISTANCE_3D_SFCGAL, "st_sfcgaldistance3d", 2),
                s(FID_ST_TRANSLATE_3D, "st_3dtranslate", 4),
                s(FID_ST_SCALE_3D, "st_3dscale", 4),
                s(FID_ST_ROTATE_3D, "st_3drotate", 5),
                // Direct sfcgal-wasm  unique surface (prefix
                // st_sfc_ to disambiguate from postgis-sfcgal).
                s(FID_SFC_AS_STL, "st_sfc_asstl", 1),
                s(FID_SFC_AS_STL_BINARY, "st_sfc_asstlbinary", 1),
                s(FID_SFC_AS_OBJ, "st_sfc_asobj", 1),
                s(FID_SFC_AS_VTK, "st_sfc_asvtk", 1),
                s(FID_SFC_ALPHA_SHAPE, "st_sfc_alphashape", 2),
                s(FID_SFC_OPTIMAL_ALPHA_SHAPE, "st_sfc_optimalalphashape", 1),
                s(FID_SFC_EXTRUDE_STRAIGHT, "st_sfc_extrudestraight", 2),
                s(FID_SFC_EXTRUDE_STRAIGHT_SKELETON, "st_sfc_extrudestraightskeleton", 2),
                s(FID_SFC_MAKE_VALID, "st_sfc_makevalid", 1),
                s(FID_SFC_IS_VALID, "st_sfc_isvalid", 1),
                s(FID_SFC_AREA, "st_sfc_area", 1),
                s(FID_SFC_VOLUME, "st_sfc_volume", 1),
                s(FID_SFC_LENGTH, "st_sfc_length", 1),
                s(FID_SFC_DISTANCE, "st_sfc_distance", 2),
                s(FID_SFC_VERSION, "st_sfc_version", 0),
                s(FID_SFC_TRIANGLE, "st_sfc_triangle", 6),
                s(FID_SFC_TESSELLATE, "st_sfc_tessellate", 1),
                s(FID_SFC_CONVEX_HULL, "st_sfc_convexhull", 1),
                s(FID_SFC_DIFFERENCE, "st_sfc_difference", 2),
                s(FID_SFC_INTERSECTION, "st_sfc_intersection", 2),
                s(FID_SFC_UNION, "st_sfc_union", 2),
                // Raster
                s(FID_RST_WIDTH, "st_rast_width", 1),
                s(FID_RST_HEIGHT, "st_rast_height", 1),
                s(FID_RST_NUM_BANDS, "st_rast_numbands", 1),
                s(FID_RST_UPPER_LEFT_X, "st_rast_upperleftx", 1),
                s(FID_RST_UPPER_LEFT_Y, "st_rast_upperlefty", 1),
                s(FID_RST_SCALE_X, "st_rast_scalex", 1),
                s(FID_RST_SCALE_Y, "st_rast_scaley", 1),
                s(FID_RST_SKEW_X, "st_rast_skewx", 1),
                s(FID_RST_SKEW_Y, "st_rast_skewy", 1),
                s(FID_RST_SRID, "st_rast_srid", 1),
                s(FID_RST_HAS_NO_BAND, "st_rast_hasnoband", 2),
                s(FID_RST_VALUE, "st_rast_value", 4),
                s(FID_RST_NEAREST_VALUE, "st_rast_nearestvalue", 4),
                s(FID_RST_PIXEL_AS_POINT, "st_rast_pixelaspoint", 3),
                s(FID_RST_PIXEL_AS_POLYGON, "st_rast_pixelaspolygon", 3),
                s(FID_RST_PIXEL_AS_CENTROID, "st_rast_pixelascentroid", 3),
                s(FID_RST_RASTER_TO_WORLD_COORD_X, "st_rast_rastertoworldcoordx", 3),
                s(FID_RST_RASTER_TO_WORLD_COORD_Y, "st_rast_rastertoworldcoordy", 3),
                s(FID_RST_AS_PNG, "st_rast_aspng", 2),
                s(FID_RST_AS_TIFF, "st_rast_astiff", 1),
                s(FID_RST_R_INTERSECTS, "st_rast_intersects", 2),
                s(FID_RST_R_CONTAINS, "st_rast_contains", 2),
                s(FID_RST_R_WITHIN, "st_rast_within", 2),
                s(FID_RST_R_COVERS, "st_rast_covers", 2),
                s(FID_RST_R_OVERLAPS, "st_rast_overlaps", 2),
                s(FID_RST_R_INTERSECTS_GEOM, "st_rast_intersectsgeom", 2),
                s(FID_RST_R_CONTAINS_GEOM, "st_rast_containsgeom", 2),
                s(FID_RST_POLYGON_FROM_RAST, "st_rast_polygon", 2),
                s(FID_RST_CONVEX_HULL, "st_rast_convexhull", 1),
                s(FID_RST_SLOPE, "st_rast_slope", 2),
                s(FID_RST_ASPECT, "st_rast_aspect", 2),
                s(FID_RST_ROUGHNESS, "st_rast_roughness", 2),
                s(FID_RST_TRI, "st_rast_tri", 2),
                s(FID_RST_TPI, "st_rast_tpi", 2),
                // Raster v2: constructors, stats, transforms
                s(FID_RST_MAKE_EMPTY, "st_rast_makeemptyraster", 9),
                s(FID_RST_ADD_BAND, "st_rast_addband", 4),
                s(FID_RST_SET_VALUE, "st_rast_setvalue", 5),
                s(FID_RST_SUMMARY_COUNT, "st_rast_count", 2),
                s(FID_RST_SUMMARY_SUM, "st_rast_sum", 2),
                s(FID_RST_SUMMARY_MEAN, "st_rast_mean", 2),
                s(FID_RST_SUMMARY_STDDEV, "st_rast_stddev", 2),
                s(FID_RST_SUMMARY_MIN, "st_rast_min", 2),
                s(FID_RST_SUMMARY_MAX, "st_rast_max", 2),
                s(FID_RST_QUANTILE, "st_rast_quantile", 3),
                s(FID_RST_WORLD_TO_RAST_X, "st_rast_worldtorastercoordx", 3),
                s(FID_RST_WORLD_TO_RAST_Y, "st_rast_worldtorastercoordy", 3),
                s(FID_RST_HILL_SHADE, "st_rast_hillshade", 4),
                s(FID_RST_RESIZE, "st_rast_resize", 4),
                s(FID_RST_RESCALE, "st_rast_rescale", 4),
                s(FID_RST_BAND_PIXEL_TYPE, "st_rast_bandpixeltype", 2),
                s(FID_RST_BAND_NODATA, "st_rast_bandnodatavalue", 2),
                // Topology (read-only accessors + topojson output)
                s(FID_TOPO_NAME, "st_topo_name", 1),
                s(FID_TOPO_SRID, "st_topo_srid", 1),
                s(FID_TOPO_PRECISION, "st_topo_precision", 1),
                s(FID_TOPO_NODE_COUNT, "st_topo_nodecount", 1),
                s(FID_TOPO_EDGE_COUNT, "st_topo_edgecount", 1),
                s(FID_TOPO_FACE_COUNT, "st_topo_facecount", 1),
                s(FID_TOPO_AS_TOPOJSON, "st_topo_astopojson", 1),
                // Spatial index (STRtree) — handle-based API.
                s(FID_STRTREE_CREATE, "st_strtree_create", 1),
                s(FID_STRTREE_INSERT, "st_strtree_insert", 3),
                s(FID_STRTREE_BUILD, "st_strtree_build", 1),
                s(FID_STRTREE_QUERY, "st_strtree_query", 5),
                s(FID_STRTREE_NEAREST, "st_strtree_nearest", 2),
                s(FID_STRTREE_KNN, "st_strtree_knn", 3),
                s(FID_STRTREE_WITHIN, "st_strtree_within", 3),
                s(FID_STRTREE_DESTROY, "st_strtree_destroy", 1),
            ],
            aggregate_functions: alloc::vec![
                AggregateFunctionSpec {
                    id: AGG_ST_UNION,
                    name: "st_union_agg".into(),
                    num_args: 1,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_POLYGONIZE,
                    name: "st_polygonize_agg".into(),
                    num_args: 1,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_MAKELINE,
                    name: "st_makeline_agg".into(),
                    num_args: 1,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_CLUSTER_INTERSECTING,
                    name: "st_clusterintersecting_agg".into(),
                    num_args: 1,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_CLUSTER_WITHIN,
                    name: "st_clusterwithin_agg".into(),
                    num_args: 2,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_EXTENT_3D,
                    name: "st_3dextent_agg".into(),
                    num_args: 1,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_CLUSTER_DBSCAN,
                    name: "st_clusterdbscan_agg".into(),
                    num_args: 3,
                    func_flags: det,
                    is_window: false,
                },
                AggregateFunctionSpec {
                    id: AGG_ST_CLUSTER_KMEANS,
                    name: "st_clusterkmeans_agg".into(),
                    num_args: 2,
                    func_flags: det,
                    is_window: false,
                },
            ],
            collations: alloc::vec![],
            vtabs: alloc::vec![],
            has_authorizer: false,
            has_update_hook: false,
            has_commit_hook: false,
            declared_capabilities: alloc::vec![],
        }
    }
}

// ───────────── Helpers ─────────────

fn arg_f64(args: &[SqlValue], idx: usize, name: &str) -> Result<f64, String> {
    match args.get(idx) {
        Some(SqlValue::Integer(i)) => Ok(*i as f64),
        Some(SqlValue::Real(r)) => Ok(*r),
        Some(SqlValue::Text(s)) => s
            .parse::<f64>()
            .map_err(|_| format!("{name}: arg {idx} not numeric")),
        _ => Err(format!("{name}: arg {idx} not numeric")),
    }
}

fn arg_i64(args: &[SqlValue], idx: usize, name: &str) -> Result<i64, String> {
    match args.get(idx) {
        Some(SqlValue::Integer(i)) => Ok(*i),
        Some(SqlValue::Real(r)) => Ok(*r as i64),
        _ => Err(format!("{name}: arg {idx} not integer")),
    }
}

fn arg_text<'a>(args: &'a [SqlValue], idx: usize, name: &str) -> Result<&'a str, String> {
    match args.get(idx) {
        Some(SqlValue::Text(s)) => Ok(s.as_str()),
        _ => Err(format!("{name}: arg {idx} must be TEXT")),
    }
}

fn arg_blob<'a>(args: &'a [SqlValue], idx: usize, name: &str) -> Result<&'a [u8], String> {
    match args.get(idx) {
        Some(SqlValue::Blob(b)) => Ok(b.as_slice()),
        Some(SqlValue::Text(s)) => Ok(s.as_bytes()),
        _ => Err(format!("{name}: arg {idx} must be BLOB")),
    }
}

fn from_wkb(bytes: &[u8], name: &str) -> Result<Geometry, String> {
    Geometry::from_wkb(bytes).map_err(|e| format!("{name}: {}", postgis_err_string(e)))
}

fn geog_from_wkb(bytes: &[u8], name: &str) -> Result<Geography, String> {
    Geography::from_wkb(bytes).map_err(|e| format!("{name}: {}", postgis_err_string(e)))
}

fn arg_to_f64(v: Option<&SqlValue>) -> Option<f64> {
    match v? {
        SqlValue::Real(r) => Some(*r),
        SqlValue::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn arg_to_i64(v: Option<&SqlValue>) -> Option<i64> {
    match v? {
        SqlValue::Integer(i) => Some(*i),
        SqlValue::Real(r) => Some(*r as i64),
        _ => None,
    }
}

fn topo_from_bytes(bytes: &[u8], name: &str) -> Result<Topology, String> {
    use bindings::postgis::wasm::postgis_topology_types as t;
    t::from_bytes(bytes).map_err(|e| format!("{name}: topology: {e:?}"))
}

// Raster reads from a BLOB containing serialized raster bytes
// (postgis-raster's interface-level `from-binary`). Mirrors the
// geometry helper.
fn rast_from_blob(bytes: &[u8], name: &str) -> Result<Raster, String> {
    use bindings::postgis::wasm::postgis_raster_types as t;
    t::from_binary(bytes).map_err(|e| format!("{name}: {}", raster_err_string(e)))
}

fn parse_pixel_type(
    s: &str,
) -> Result<bindings::postgis::wasm::postgis_raster_types::PixelType, String> {
    use bindings::postgis::wasm::postgis_raster_types::PixelType as P;
    Ok(match s.to_ascii_lowercase().as_str() {
        "bool1" | "bool" | "1bb" => P::Bool1,
        "uint8" | "u8" | "8bui" => P::Uint8,
        "int8" | "i8" | "8bsi" => P::Int8,
        "uint16" | "u16" | "16bui" => P::Uint16,
        "int16" | "i16" | "16bsi" => P::Int16,
        "uint32" | "u32" | "32bui" => P::Uint32,
        "int32" | "i32" | "32bsi" => P::Int32,
        "float32" | "f32" | "32bf" => P::Float32,
        "float64" | "f64" | "64bf" => P::Float64,
        other => return Err(format!("unknown pixel-type {other:?}")),
    })
}

fn pixel_type_str(
    p: bindings::postgis::wasm::postgis_raster_types::PixelType,
) -> &'static str {
    use bindings::postgis::wasm::postgis_raster_types::PixelType as P;
    match p {
        P::Bool1 => "1BB",
        P::Uint8 => "8BUI",
        P::Int8 => "8BSI",
        P::Uint16 => "16BUI",
        P::Int16 => "16BSI",
        P::Uint32 => "32BUI",
        P::Int32 => "32BSI",
        P::Float32 => "32BF",
        P::Float64 => "64BF",
    }
}

fn raster_err_string(
    e: bindings::postgis::wasm::postgis_raster_types::RasterError,
) -> String {
    use bindings::postgis::wasm::postgis_raster_types::RasterError as E;
    match e {
        E::ParseError(s) => format!("parse error: {s}"),
        E::OutOfBounds(s) => format!("out of bounds: {s}"),
        E::TypeMismatch(s) => format!("type mismatch: {s}"),
        E::General(s) => s,
    }
}

// ── sfcgal-wasm helpers ──────────────────────────────────────

thread_local! {
    static SFC_INITED: RefCell<bool> = const { RefCell::new(false) };
}

fn sfc_ensure_init() {
    // sfcgal-wasm auto-initializes on first call into a
    // geometry / io fn (the world-level `init` export is not
    // importable through an interface boundary). Kept as a
    // no-op so the call-site comment still reads well.
    SFC_INITED.with(|c| { *c.borrow_mut() = true; });
}

/// Decode a `geometry-result` (handle or sfcgal-error).
fn sfc_geom(r: sf_geom::GeometryResult, name: &str) -> Result<u64, String> {
    match r {
        sf_geom::GeometryResult::Ok(h) => Ok(h),
        sf_geom::GeometryResult::Err(e) => Err(format!("{name}: sfcgal {}: {}", e.code, e.message)),
    }
}

fn sfc_string(r: sf_geom::StringResult, name: &str) -> Result<String, String> {
    match r {
        sf_geom::StringResult::Ok(s) => Ok(s),
        sf_geom::StringResult::Err(e) => Err(format!("{name}: sfcgal {}: {}", e.code, e.message)),
    }
}

fn sfc_f64(r: sf_geom::F64Result, name: &str) -> Result<f64, String> {
    match r {
        sf_geom::F64Result::Ok(v) => Ok(v),
        sf_geom::F64Result::Err(e) => Err(format!("{name}: sfcgal {}: {}", e.code, e.message)),
    }
}

/// Read a WKB BLOB into an sfcgal geometry handle (RAII-ish
/// caller MUST call sf_geom::destroy when done).
fn sfc_read_wkb(bytes: &[u8], name: &str) -> Result<u64, String> {
    sfc_ensure_init();
    sfc_geom(sf_io::read_wkb(bytes), name)
}

/// Serialize a handle to WKB bytes (NDR) and destroy.
fn sfc_take_wkb(handle: u64) -> Vec<u8> {
    let bytes = sf_io::write_wkb(handle, sf_io::WkbByteOrder::LittleEndian);
    sf_geom::destroy(handle);
    bytes
}

fn postgis_err_string(e: bindings::postgis::wasm::postgis_types::PostgisError) -> String {
    use bindings::postgis::wasm::postgis_types::PostgisError as E;
    match e {
        E::InvalidGeometry(s) => format!("invalid geometry: {s}"),
        E::ParseError(s) => format!("parse error: {s}"),
        E::UnsupportedOperation(s) => format!("unsupported: {s}"),
        E::NumericError(s) => format!("numeric: {s}"),
        E::SridMismatch(s) => format!("SRID mismatch: {s}"),
        E::General(s) => s,
    }
}

// ───────────── Dispatch macros ─────────────

/// f(geom) -> Result<f64>  most accessors / measurements
macro_rules! g_to_f64 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Real(r))
    }};
}

/// f(geom) -> u32  infallible counts.
macro_rules! g_to_int {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> Result<u32>  fallible counts.
macro_rules! g_to_int_result {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Integer(r as i64))
    }};
}

/// f(geom) -> bool  infallible is-X predicates.
macro_rules! g_to_bool {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> string  as-text / as-geojson / etc.
macro_rules! g_to_string {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Text($module::$fn(&g)))
    }};
}

/// f(geom) -> Result<string>  fallible string outputs.
macro_rules! g_to_string_result {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let s = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Text(s))
    }};
}

/// f(geom) -> list<u8>  as-binary / as-ewkb.
macro_rules! g_to_blob {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Blob($module::$fn(&g)))
    }};
}

/// f(geom) -> Result<geometry>
macro_rules! g_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(geom) -> geometry  infallible.
macro_rules! g_to_geom_inf {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Blob($module::$fn(&g).as_wkb()))
    }};
}

/// f(geom1, geom2) -> Result<f64>
macro_rules! gg_to_f64 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Real(r))
    }};
}

/// f(geom1, geom2) -> Result<bool>
macro_rules! gg_to_bool {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Integer(r as i64))
    }};
}

/// f(geom1, geom2) -> Result<geometry>
macro_rules! gg_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(geom, f64) -> Result<geometry>  buffer/simplify shape.
macro_rules! gd_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let d = arg_f64(&$args, 1, $name)?;
        let r = $module::$fn(&g, d)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(geom, f64, f64) -> Result<geometry>  translate/scale shape.
macro_rules! gff_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let a = arg_f64(&$args, 1, $name)?;
        let b = arg_f64(&$args, 2, $name)?;
        let r = $module::$fn(&g, a, b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(g1, g2, f64) -> Result<bool>  dwithin shape.
macro_rules! ggd_to_bool {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let d = arg_f64(&$args, 2, $name)?;
        let r = $module::$fn(&a, &b, d)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Integer(r as i64))
    }};
}

/// f(g1, g2, f64) -> Result<geometry>  snap shape.
macro_rules! ggd_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let d = arg_f64(&$args, 2, $name)?;
        let r = $module::$fn(&a, &b, d)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(r.as_wkb()))
    }};
}

/// f(g1, g2) -> Result<string>  st_relate.
macro_rules! gg_to_string_result {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        let r = $module::$fn(&a, &b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Text(r))
    }};
}

/// f(g1, g2) -> Result<f64>  distance variants.
macro_rules! gg_to_f64_inf {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let a = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let b = from_wkb(arg_blob(&$args, 1, $name)?, $name)?;
        Ok(SqlValue::Real($module::$fn(&a, &b)))
    }};
}

/// f(geom) -> bool  infallible has_*.
macro_rules! g_to_bool_inf {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> Result<bool>  is_polygon_cw etc.
macro_rules! g_to_bool_result {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        let r = $module::$fn(&g)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Integer(r as i64))
    }};
}

/// f(geom) -> u32  ndims / coord-dim.
macro_rules! g_to_u32 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> s32  dimension.
macro_rules! g_to_s32 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(geom) -> u64  mem_size.
macro_rules! g_to_u64 {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let g = from_wkb(arg_blob(&$args, 0, $name)?, $name)?;
        Ok(SqlValue::Integer($module::$fn(&g) as i64))
    }};
}

/// f(wkt) -> Result<geometry>  parsers.
macro_rules! text_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let t = arg_text(&$args, 0, $name)?;
        let g = $module::$fn(t)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(g.as_wkb()))
    }};
}

/// f(wkb) -> Result<geometry>  WKB parsers.
macro_rules! blob_to_geom {
    ($args:expr, $name:expr, $module:ident :: $fn:ident) => {{
        let b = arg_blob(&$args, 0, $name)?;
        let g = $module::$fn(b)
            .map_err(|e| format!("{}: {}", $name, postgis_err_string(e)))?;
        Ok(SqlValue::Blob(g.as_wkb()))
    }};
}

// ───────────── Dispatch ─────────────

impl ScalarFunctionGuest for PostgisBridge {
    fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
        if args.iter().any(|v| matches!(v, SqlValue::Null)) {
            return Ok(SqlValue::Null);
        }
        match func_id {
            // ── Constructors ──
            FID_ST_MAKEPOINT | FID_ST_POINT => {
                let x = arg_f64(&args, 0, "st_makepoint")?;
                let y = arg_f64(&args, 1, "st_makepoint")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point(x, y).as_wkb()))
            }
            FID_ST_MAKEPOINT_Z | FID_ST_POINT_Z => {
                let x = arg_f64(&args, 0, "st_makepointz")?;
                let y = arg_f64(&args, 1, "st_makepointz")?;
                let z = arg_f64(&args, 2, "st_makepointz")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point_z(x, y, z).as_wkb()))
            }
            FID_ST_MAKEPOINT_M | FID_ST_POINT_M => {
                let x = arg_f64(&args, 0, "st_makepointm")?;
                let y = arg_f64(&args, 1, "st_makepointm")?;
                let m = arg_f64(&args, 2, "st_makepointm")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point_m(x, y, m).as_wkb()))
            }
            FID_ST_MAKEPOINT_ZM | FID_ST_POINT_ZM => {
                let x = arg_f64(&args, 0, "st_makepointzm")?;
                let y = arg_f64(&args, 1, "st_makepointzm")?;
                let z = arg_f64(&args, 2, "st_makepointzm")?;
                let m = arg_f64(&args, 3, "st_makepointzm")?;
                Ok(SqlValue::Blob(pg_ctor::st_make_point_zm(x, y, z, m).as_wkb()))
            }
            FID_ST_MAKE_ENVELOPE => {
                let xmin = arg_f64(&args, 0, "st_makeenvelope")?;
                let ymin = arg_f64(&args, 1, "st_makeenvelope")?;
                let xmax = arg_f64(&args, 2, "st_makeenvelope")?;
                let ymax = arg_f64(&args, 3, "st_makeenvelope")?;
                Ok(SqlValue::Blob(
                    pg_ctor::st_make_envelope(xmin, ymin, xmax, ymax).as_wkb(),
                ))
            }
            FID_ST_MAKE_ENVELOPE_SRID => {
                let xmin = arg_f64(&args, 0, "st_makeenvelope_srid")?;
                let ymin = arg_f64(&args, 1, "st_makeenvelope_srid")?;
                let xmax = arg_f64(&args, 2, "st_makeenvelope_srid")?;
                let ymax = arg_f64(&args, 3, "st_makeenvelope_srid")?;
                let srid = arg_i64(&args, 4, "st_makeenvelope_srid")? as i32;
                Ok(SqlValue::Blob(
                    pg_ctor::st_make_envelope_srid(xmin, ymin, xmax, ymax, srid).as_wkb(),
                ))
            }
            FID_ST_GEOMFROMTEXT => {
                let wkt = arg_text(&args, 0, "st_geomfromtext")?;
                let g = pg_ctor::st_geom_from_text(wkt)
                    .map_err(|e| format!("st_geomfromtext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMTEXT_SRID => {
                let wkt = arg_text(&args, 0, "st_geomfromtext_srid")?;
                let srid = arg_i64(&args, 1, "st_geomfromtext_srid")? as i32;
                let g = pg_ctor::st_geom_from_text_srid(wkt, srid)
                    .map_err(|e| format!("st_geomfromtext_srid: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMEWKT => {
                let wkt = arg_text(&args, 0, "st_geomfromewkt")?;
                let g = pg_ctor::st_geom_from_ewkt(wkt)
                    .map_err(|e| format!("st_geomfromewkt: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_POINTFROMTEXT => {
                let wkt = arg_text(&args, 0, "st_pointfromtext")?;
                let g = pg_ctor::st_point_from_text(wkt)
                    .map_err(|e| format!("st_pointfromtext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMWKB => {
                let wkb = arg_blob(&args, 0, "st_geomfromwkb")?;
                let g = from_wkb(wkb, "st_geomfromwkb")?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOMFROMGEOJSON => {
                let s = arg_text(&args, 0, "st_geomfromgeojson")?;
                let g = Geometry::from_geojson(s)
                    .map_err(|e| format!("st_geomfromgeojson: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_MAKE_LINE_TWO => {
                let a = from_wkb(arg_blob(&args, 0, "st_makeline")?, "st_makeline")?;
                let b = from_wkb(arg_blob(&args, 1, "st_makeline")?, "st_makeline")?;
                let g = pg_ctor::st_make_line_two(&a, &b)
                    .map_err(|e| format!("st_makeline: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }

            // ── Accessors ──
            FID_ST_X => g_to_f64!(args, "st_x", pg_acc::st_x),
            FID_ST_Y => g_to_f64!(args, "st_y", pg_acc::st_y),
            FID_ST_XMIN => g_to_f64!(args, "st_xmin", pg_acc::st_xmin),
            FID_ST_XMAX => g_to_f64!(args, "st_xmax", pg_acc::st_xmax),
            FID_ST_YMIN => g_to_f64!(args, "st_ymin", pg_acc::st_ymin),
            FID_ST_YMAX => g_to_f64!(args, "st_ymax", pg_acc::st_ymax),
            FID_ST_SRID => {
                let g = from_wkb(arg_blob(&args, 0, "st_srid")?, "st_srid")?;
                Ok(match g.srid() {
                    Some(s) => SqlValue::Integer(s as i64),
                    None => SqlValue::Null,
                })
            }
            FID_ST_GEOMETRY_TYPE => {
                let g = from_wkb(arg_blob(&args, 0, "st_geometrytype")?, "st_geometrytype")?;
                let name = match g.geometry_type() {
                    bindings::postgis::wasm::postgis_types::GeometryType::Point => "POINT",
                    bindings::postgis::wasm::postgis_types::GeometryType::LineString => "LINESTRING",
                    bindings::postgis::wasm::postgis_types::GeometryType::Polygon => "POLYGON",
                    bindings::postgis::wasm::postgis_types::GeometryType::MultiPoint => "MULTIPOINT",
                    bindings::postgis::wasm::postgis_types::GeometryType::MultiLineString => "MULTILINESTRING",
                    bindings::postgis::wasm::postgis_types::GeometryType::MultiPolygon => "MULTIPOLYGON",
                    bindings::postgis::wasm::postgis_types::GeometryType::GeometryCollection => "GEOMETRYCOLLECTION",
                };
                Ok(SqlValue::Text(format!("ST_{name}").to_string()))
            }
            FID_ST_IS_EMPTY => {
                let g = from_wkb(arg_blob(&args, 0, "st_isempty")?, "st_isempty")?;
                Ok(SqlValue::Integer(g.is_empty() as i64))
            }
            FID_ST_IS_VALID => g_to_bool!(args, "st_isvalid", pg_pred::st_is_valid),
            FID_ST_IS_SIMPLE => g_to_bool!(args, "st_issimple", pg_pred::st_is_simple),
            FID_ST_IS_CLOSED => g_to_bool!(args, "st_isclosed", pg_pred::st_is_closed),
            FID_ST_IS_RING => g_to_bool!(args, "st_isring", pg_pred::st_is_ring),
            FID_ST_NUM_POINTS => g_to_int!(args, "st_numpoints", pg_acc::st_num_points),
            FID_ST_NUM_GEOMETRIES => g_to_int!(args, "st_numgeometries", pg_acc::st_num_geometries),
            FID_ST_NUM_INTERIOR_RINGS => g_to_int_result!(args, "st_numinteriorrings", pg_acc::st_num_interior_rings),
            FID_ST_NPOINTS => g_to_int!(args, "st_npoints", pg_acc::st_npoints),
            FID_ST_EXTERIOR_RING => g_to_geom!(args, "st_exteriorring", pg_acc::st_exterior_ring),
            FID_ST_INTERIOR_RING_N => {
                let g = from_wkb(arg_blob(&args, 0, "st_interiorringn")?, "st_interiorringn")?;
                let n = arg_i64(&args, 1, "st_interiorringn")? as u32;
                let r = pg_acc::st_interior_ring_n(&g, n)
                    .map_err(|e| format!("st_interiorringn: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_POINT_N => {
                let g = from_wkb(arg_blob(&args, 0, "st_pointn")?, "st_pointn")?;
                let n = arg_i64(&args, 1, "st_pointn")? as u32;
                let r = pg_acc::st_point_n(&g, n)
                    .map_err(|e| format!("st_pointn: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOMETRY_N => {
                let g = from_wkb(arg_blob(&args, 0, "st_geometryn")?, "st_geometryn")?;
                let n = arg_i64(&args, 1, "st_geometryn")? as u32;
                let r = pg_acc::st_geometry_n(&g, n)
                    .map_err(|e| format!("st_geometryn: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_START_POINT => g_to_geom!(args, "st_startpoint", pg_acc::st_start_point),
            FID_ST_END_POINT => g_to_geom!(args, "st_endpoint", pg_acc::st_end_point),
            FID_ST_BOUNDARY => g_to_geom!(args, "st_boundary", pg_proc::st_boundary),
            FID_ST_ENVELOPE => g_to_geom!(args, "st_envelope", pg_acc::st_envelope),
            FID_ST_SET_SRID => {
                let g = from_wkb(arg_blob(&args, 0, "st_setsrid")?, "st_setsrid")?;
                let srid = arg_i64(&args, 1, "st_setsrid")? as i32;
                Ok(SqlValue::Blob(g.set_srid(srid).as_wkb()))
            }

            // ── Measurements ──
            FID_ST_AREA => g_to_f64!(args, "st_area", pg_meas::st_area),
            FID_ST_LENGTH => g_to_f64!(args, "st_length", pg_meas::st_length),
            FID_ST_PERIMETER => g_to_f64!(args, "st_perimeter", pg_meas::st_perimeter),
            FID_ST_LENGTH_TWOD => g_to_f64!(args, "st_length2d", pg_meas::st_length_twod),
            FID_ST_LENGTH_THREED => g_to_f64!(args, "st_length3d", pg_meas::st_length_threed),
            FID_ST_PERIMETER_THREED => g_to_f64!(args, "st_perimeter3d", pg_meas::st_perimeter_threed),
            FID_ST_DISTANCE => gg_to_f64!(args, "st_distance", pg_meas::st_distance),
            FID_ST_DISTANCE_THREED => gg_to_f64!(args, "st_distance3d", pg_meas::st_distance_threed),
            FID_ST_MAX_DISTANCE => gg_to_f64!(args, "st_maxdistance", pg_meas::st_max_distance),
            FID_ST_MAX_DISTANCE_THREED => gg_to_f64!(args, "st_maxdistance3d", pg_meas::st_max_distance_threed),
            FID_ST_HAUSDORFF_DISTANCE => gg_to_f64!(args, "st_hausdorffdistance", pg_meas::st_hausdorff_distance),
            FID_ST_FRECHET_DISTANCE => gg_to_f64!(args, "st_frechetdistance", pg_meas::st_frechet_distance),

            // ── Predicates ──
            FID_ST_INTERSECTS => gg_to_bool!(args, "st_intersects", pg_pred::st_intersects),
            FID_ST_CONTAINS => gg_to_bool!(args, "st_contains", pg_pred::st_contains),
            FID_ST_WITHIN => gg_to_bool!(args, "st_within", pg_pred::st_within),
            FID_ST_EQUALS => gg_to_bool!(args, "st_equals", pg_pred::st_equals),
            FID_ST_DISJOINT => gg_to_bool!(args, "st_disjoint", pg_pred::st_disjoint),
            FID_ST_OVERLAPS => gg_to_bool!(args, "st_overlaps", pg_pred::st_overlaps),
            FID_ST_TOUCHES => gg_to_bool!(args, "st_touches", pg_pred::st_touches),
            FID_ST_CROSSES => gg_to_bool!(args, "st_crosses", pg_pred::st_crosses),
            FID_ST_COVERED_BY => gg_to_bool!(args, "st_coveredby", pg_pred::st_covered_by),
            FID_ST_COVERS => gg_to_bool!(args, "st_covers", pg_pred::st_covers),
            FID_ST_CONTAINS_PROPERLY => gg_to_bool!(args, "st_containsproperly", pg_pred::st_contains_properly),
            FID_ST_3D_INTERSECTS => gg_to_bool!(args, "st_3dintersects", pg_pred::st_intersects_threed),
            // st-3d-disjoint isn't exported by postgis-wasm; alias to st-disjoint.
            FID_ST_3D_DISJOINT => gg_to_bool!(args, "st_3ddisjoint", pg_pred::st_disjoint),

            // ── Processing ──
            FID_ST_BUFFER => gd_to_geom!(args, "st_buffer", pg_proc::st_buffer),
            FID_ST_INTERSECTION => gg_to_geom!(args, "st_intersection", pg_proc::st_intersection),
            FID_ST_UNION => gg_to_geom!(args, "st_union", pg_proc::st_union),
            FID_ST_DIFFERENCE => gg_to_geom!(args, "st_difference", pg_proc::st_difference),
            FID_ST_SYM_DIFFERENCE => gg_to_geom!(args, "st_symdifference", pg_proc::st_sym_difference),
            FID_ST_UNARY_UNION => g_to_geom!(args, "st_unaryunion", pg_proc::st_unary_union),
            FID_ST_SIMPLIFY => gd_to_geom!(args, "st_simplify", pg_proc::st_simplify),
            FID_ST_SIMPLIFY_PT => gd_to_geom!(args, "st_simplifypreservetopology", pg_proc::st_simplify_preserve_topology),
            FID_ST_SIMPLIFY_VW => gd_to_geom!(args, "st_simplifyvw", pg_proc::st_simplify_vw),
            FID_ST_CONVEX_HULL => g_to_geom!(args, "st_convexhull", pg_proc::st_convex_hull),
            FID_ST_CONCAVE_HULL => gd_to_geom!(args, "st_concavehull", pg_proc::st_concave_hull),
            FID_ST_CENTROID => g_to_geom!(args, "st_centroid", pg_proc::st_centroid),
            FID_ST_POINT_ON_SURFACE => g_to_geom!(args, "st_pointonsurface", pg_proc::st_point_on_surface),
            FID_ST_ORIENTED_ENVELOPE => g_to_geom!(args, "st_orientedenvelope", pg_proc::st_oriented_envelope),
            FID_ST_MIN_BOUNDING_CIRCLE => g_to_geom!(args, "st_minimumboundingcircle", pg_proc::st_minimum_bounding_circle),
            FID_ST_LINE_MERGE => g_to_geom!(args, "st_linemerge", pg_proc::st_line_merge),
            FID_ST_MAKE_VALID => g_to_geom!(args, "st_makevalid", pg_proc::st_make_valid),
            FID_ST_REVERSE => g_to_geom!(args, "st_reverse", pg_proc::st_reverse),
            FID_ST_FLIP_COORDINATES => g_to_geom!(args, "st_flipcoordinates", pg_xform::st_flip_coordinates),
            FID_ST_FORCE_2D => g_to_geom_inf!(args, "st_force2d", pg_xform::st_force_twod),
            FID_ST_FORCE_3D => g_to_geom_inf!(args, "st_force3d", pg_xform::st_force_threed),
            FID_ST_MULTI => g_to_geom!(args, "st_multi", pg_acc::st_multi),
            FID_ST_COLLECTION_HOMOGENIZE => g_to_geom!(args, "st_collectionhomogenize", pg_acc::st_collection_homogenize),

            // ── Output ──
            FID_ST_ASTEXT => g_to_string!(args, "st_astext", pg_out::st_as_text),
            FID_ST_ASBINARY => g_to_blob!(args, "st_asbinary", pg_out::st_as_binary),
            FID_ST_AS_EWKT => g_to_string!(args, "st_asewkt", pg_out::st_as_ewkt),
            FID_ST_AS_EWKB => g_to_blob!(args, "st_asewkb", pg_out::st_as_ewkb),
            FID_ST_AS_HEXEWKB => g_to_string!(args, "st_ashexewkb", pg_out::st_as_hexewkb),
            FID_ST_AS_GEOJSON => g_to_string!(args, "st_asgeojson", pg_out::st_as_geojson),
            FID_ST_AS_SVG => g_to_string_result!(args, "st_assvg", pg_out::st_as_svg),
            FID_ST_AS_KML => g_to_string_result!(args, "st_askml", pg_out::st_as_kml),
            FID_ST_AS_GML => {
                let g = from_wkb(arg_blob(&args, 0, "st_asgml")?, "st_asgml")?;
                let s = pg_out::st_as_gml(&g, None, None)
                    .map_err(|e| format!("st_asgml: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Text(s))
            }
            FID_ST_AS_X3D => g_to_string_result!(args, "st_asx3d", pg_out::st_as_x3d),
            FID_ST_SUMMARY => g_to_string!(args, "st_summary", pg_out::st_summary),
            FID_ST_GEOHASH => {
                let g = from_wkb(arg_blob(&args, 0, "st_geohash")?, "st_geohash")?;
                let s = pg_out::st_geohash(&g, None)
                    .map_err(|e| format!("st_geohash: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Text(s))
            }

            // ── v2 batch (accessors) ──
            FID_ST_Z => {
                let g = from_wkb(arg_blob(&args, 0, "st_z")?, "st_z")?;
                Ok(match pg_acc::st_z(&g)
                    .map_err(|e| format!("st_z: {}", postgis_err_string(e)))?
                {
                    Some(z) => SqlValue::Real(z),
                    None => SqlValue::Null,
                })
            }
            FID_ST_M => {
                let g = from_wkb(arg_blob(&args, 0, "st_m")?, "st_m")?;
                Ok(match pg_acc::st_m(&g)
                    .map_err(|e| format!("st_m: {}", postgis_err_string(e)))?
                {
                    Some(z) => SqlValue::Real(z),
                    None => SqlValue::Null,
                })
            }
            FID_ST_ZMIN => {
                let g = from_wkb(arg_blob(&args, 0, "st_zmin")?, "st_zmin")?;
                Ok(match pg_acc::st_zmin(&g)
                    .map_err(|e| format!("st_zmin: {}", postgis_err_string(e)))?
                {
                    Some(z) => SqlValue::Real(z),
                    None => SqlValue::Null,
                })
            }
            FID_ST_ZMAX => {
                let g = from_wkb(arg_blob(&args, 0, "st_zmax")?, "st_zmax")?;
                Ok(match pg_acc::st_zmax(&g)
                    .map_err(|e| format!("st_zmax: {}", postgis_err_string(e)))?
                {
                    Some(z) => SqlValue::Real(z),
                    None => SqlValue::Null,
                })
            }
            FID_ST_MMIN => {
                let g = from_wkb(arg_blob(&args, 0, "st_mmin")?, "st_mmin")?;
                Ok(match pg_acc::st_mmin(&g)
                    .map_err(|e| format!("st_mmin: {}", postgis_err_string(e)))?
                {
                    Some(z) => SqlValue::Real(z),
                    None => SqlValue::Null,
                })
            }
            FID_ST_MMAX => {
                let g = from_wkb(arg_blob(&args, 0, "st_mmax")?, "st_mmax")?;
                Ok(match pg_acc::st_mmax(&g)
                    .map_err(|e| format!("st_mmax: {}", postgis_err_string(e)))?
                {
                    Some(z) => SqlValue::Real(z),
                    None => SqlValue::Null,
                })
            }
            FID_ST_NRINGS => g_to_int!(args, "st_nrings", pg_acc::st_nrings),
            FID_ST_DIMENSION => g_to_s32!(args, "st_dimension", pg_acc::st_dimension),
            FID_ST_COORD_DIM => g_to_u32!(args, "st_coorddim", pg_acc::st_coord_dim),
            FID_ST_NDIMS => g_to_u32!(args, "st_ndims", pg_acc::st_ndims),
            FID_ST_ZMFLAG => g_to_u32!(args, "st_zmflag", pg_acc::st_zmflag),
            FID_ST_MEM_SIZE => g_to_u64!(args, "st_memsize", pg_acc::st_mem_size),
            FID_ST_IS_COLLECTION => g_to_bool_inf!(args, "st_iscollection", pg_acc::st_is_collection),
            FID_ST_HAS_ARC_ACC => g_to_bool_inf!(args, "st_hasarc", pg_acc::st_has_arc),
            FID_ST_POINTS => {
                let g = from_wkb(arg_blob(&args, 0, "st_points")?, "st_points")?;
                Ok(SqlValue::Blob(pg_acc::st_points(&g).as_wkb()))
            }
            FID_ST_BOUNDING_DIAGONAL => g_to_geom!(args, "st_boundingdiagonal", pg_acc::st_bounding_diagonal),
            FID_ST_EXPAND => gd_to_geom!(args, "st_expand", pg_acc::st_expand),
            FID_ST_COLLECTION_EXTRACT => {
                let g = from_wkb(arg_blob(&args, 0, "st_collectionextract")?, "st_collectionextract")?;
                let n = arg_i64(&args, 1, "st_collectionextract")? as u32;
                let r = pg_acc::st_collection_extract(&g, n)
                    .map_err(|e| format!("st_collectionextract: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }

            // ── v2 batch (measurements) ──
            FID_ST_CLOSEST_POINT => gg_to_geom!(args, "st_closestpoint", pg_meas::st_closest_point),
            FID_ST_CLOSEST_POINT_3D => gg_to_geom!(args, "st_3dclosestpoint", pg_meas::st_closest_point_threed),
            FID_ST_SHORTEST_LINE => gg_to_geom!(args, "st_shortestline", pg_meas::st_shortest_line),
            FID_ST_SHORTEST_LINE_3D => gg_to_geom!(args, "st_3dshortestline", pg_meas::st_shortest_line_threed),
            FID_ST_LONGEST_LINE => gg_to_geom!(args, "st_longestline", pg_meas::st_longest_line),
            FID_ST_LONGEST_LINE_3D => gg_to_geom!(args, "st_3dlongestline", pg_meas::st_longest_line_threed),
            FID_ST_AZIMUTH => gg_to_f64!(args, "st_azimuth", pg_meas::st_azimuth),
            FID_ST_ANGLE => {
                let a = from_wkb(arg_blob(&args, 0, "st_angle")?, "st_angle")?;
                let b = from_wkb(arg_blob(&args, 1, "st_angle")?, "st_angle")?;
                let c = from_wkb(arg_blob(&args, 2, "st_angle")?, "st_angle")?;
                let r = pg_meas::st_angle(&a, &b, &c)
                    .map_err(|e| format!("st_angle: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Real(r))
            }
            FID_ST_MIN_CLEARANCE => g_to_f64!(args, "st_minimumclearance", pg_meas::st_minimum_clearance),
            FID_ST_MIN_CLEARANCE_LINE => g_to_geom!(args, "st_minimumclearanceline", pg_meas::st_minimum_clearance_line),
            FID_ST_DISTANCE_CPA => gg_to_f64!(args, "st_distancecpa", pg_meas::st_distance_cpa),
            FID_ST_DISTANCE_SPHEROID => gg_to_f64!(args, "st_distancespheroid", pg_meas::st_distance_spheroid),
            FID_ST_LENGTH_SPHEROID => g_to_f64!(args, "st_lengthspheroid", pg_meas::st_length_spheroid),

            // ── v2 batch (predicates) ──
            FID_ST_DWITHIN => ggd_to_bool!(args, "st_dwithin", pg_pred::st_dwithin),
            FID_ST_DWITHIN_3D => ggd_to_bool!(args, "st_3ddwithin", pg_pred::st_dwithin_threed),
            FID_ST_DFULLY_WITHIN => ggd_to_bool!(args, "st_dfullywithin", pg_pred::st_dfully_within),
            FID_ST_EQUALS_EXACT => ggd_to_bool!(args, "st_equalsexact", pg_pred::st_equals_exact),
            FID_ST_RELATE => gg_to_string_result!(args, "st_relate", pg_pred::st_relate),
            FID_ST_RELATE_MATCH => {
                let a = from_wkb(arg_blob(&args, 0, "st_relatematch")?, "st_relatematch")?;
                let b = from_wkb(arg_blob(&args, 1, "st_relatematch")?, "st_relatematch")?;
                let p = arg_text(&args, 2, "st_relatematch")?;
                let r = pg_pred::st_relate_match(&a, &b, p)
                    .map_err(|e| format!("st_relatematch: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Integer(r as i64))
            }
            FID_ST_ORDERING_EQUALS => {
                let a = from_wkb(arg_blob(&args, 0, "st_orderingequals")?, "st_orderingequals")?;
                let b = from_wkb(arg_blob(&args, 1, "st_orderingequals")?, "st_orderingequals")?;
                Ok(SqlValue::Integer(pg_pred::st_ordering_equals(&a, &b) as i64))
            }
            FID_ST_HAS_Z => g_to_bool_inf!(args, "st_hasz", pg_pred::st_has_z),
            FID_ST_HAS_M => g_to_bool_inf!(args, "st_hasm", pg_pred::st_has_m),
            FID_ST_IS_POLYGON_CW => g_to_bool_result!(args, "st_ispolygoncw", pg_pred::st_is_polygon_cw),
            FID_ST_IS_POLYGON_CCW => g_to_bool_result!(args, "st_ispolygonccw", pg_pred::st_is_polygon_ccw),
            FID_ST_IS_VALID_TRAJECTORY => g_to_bool_result!(args, "st_isvalidtrajectory", pg_pred::st_is_valid_trajectory),
            FID_ST_POINT_INSIDE_CIRCLE => {
                let p = from_wkb(arg_blob(&args, 0, "st_pointinsidecircle")?, "st_pointinsidecircle")?;
                let cx = arg_f64(&args, 1, "st_pointinsidecircle")?;
                let cy = arg_f64(&args, 2, "st_pointinsidecircle")?;
                let r = arg_f64(&args, 3, "st_pointinsidecircle")?;
                Ok(SqlValue::Integer(
                    pg_pred::st_point_inside_circle(&p, cx, cy, r) as i64,
                ))
            }
            FID_ST_CONTAINS_3D => gg_to_bool!(args, "st_3dcontains", pg_pred::st_contains_threed),
            FID_ST_CPA_WITHIN => ggd_to_bool!(args, "st_cpawithin", pg_pred::st_cpa_within),

            // ── v2 batch (processing) ──
            FID_ST_CHAIKIN_SMOOTHING => {
                let g = from_wkb(arg_blob(&args, 0, "st_chaikinsmoothing")?, "st_chaikinsmoothing")?;
                let n = arg_i64(&args, 1, "st_chaikinsmoothing")? as u32;
                let r = pg_proc::st_chaikin_smoothing(&g, n)
                    .map_err(|e| format!("st_chaikinsmoothing: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_FORCE_RHR => g_to_geom!(args, "st_forcerhr", pg_proc::st_force_rhr),
            FID_ST_NORMALIZE => g_to_geom!(args, "st_normalize", pg_proc::st_normalize),
            FID_ST_REMOVE_REPEATED_POINTS => {
                let g = from_wkb(arg_blob(&args, 0, "st_removerepeatedpoints")?, "st_removerepeatedpoints")?;
                let r = pg_proc::st_remove_repeated_points(&g, None)
                    .map_err(|e| format!("st_removerepeatedpoints: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_SNAP_TO_GRID => gd_to_geom!(args, "st_snaptogrid", pg_proc::st_snap_to_grid),
            FID_ST_SNAP => ggd_to_geom!(args, "st_snap", pg_proc::st_snap),
            FID_ST_REDUCE_PRECISION => gd_to_geom!(args, "st_reduceprecision", pg_proc::st_reduce_precision),
            FID_ST_LINE_MERGE_DIRECTED => {
                let g = from_wkb(arg_blob(&args, 0, "st_linemergedirected")?, "st_linemergedirected")?;
                let d = arg_i64(&args, 1, "st_linemergedirected")? != 0;
                let r = pg_proc::st_line_merge_directed(&g, d)
                    .map_err(|e| format!("st_linemergedirected: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_OFFSET_CURVE => gd_to_geom!(args, "st_offsetcurve", pg_proc::st_offset_curve),
            FID_ST_SHARED_PATHS => gg_to_geom!(args, "st_sharedpaths", pg_proc::st_shared_paths),
            FID_ST_VORONOI_POLYGONS => gd_to_geom!(args, "st_voronoipolygons", pg_proc::st_voronoi_polygons),
            FID_ST_VORONOI_LINES => gd_to_geom!(args, "st_voronoilines", pg_proc::st_voronoi_lines),
            FID_ST_DELAUNAY_TRIANGLES => gd_to_geom!(args, "st_delaunaytriangles", pg_proc::st_delaunay_triangles),
            FID_ST_CONSTRAINED_DELAUNAY => g_to_geom!(args, "st_constraineddelaunaytriangles", pg_proc::st_constrained_delaunay_triangles),
            FID_ST_GENERATE_POINTS => {
                let g = from_wkb(arg_blob(&args, 0, "st_generatepoints")?, "st_generatepoints")?;
                let n = arg_i64(&args, 1, "st_generatepoints")? as u32;
                let r = pg_proc::st_generate_points(&g, n)
                    .map_err(|e| format!("st_generatepoints: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_SEGMENTIZE => gd_to_geom!(args, "st_segmentize", pg_proc::st_segmentize),
            FID_ST_FORCE_POLYGON_CW => g_to_geom!(args, "st_forcepolygoncw", pg_proc::st_force_polygon_cw),
            FID_ST_FORCE_POLYGON_CCW => g_to_geom!(args, "st_forcepolygonccw", pg_proc::st_force_polygon_ccw),
            FID_ST_SPLIT => gg_to_geom!(args, "st_split", pg_proc::st_split),
            FID_ST_NODE => g_to_geom!(args, "st_node", pg_proc::st_node),
            FID_ST_POLYGONIZE => g_to_geom!(args, "st_polygonize", pg_proc::st_polygonize),
            FID_ST_BUILD_AREA => g_to_geom!(args, "st_buildarea", pg_proc::st_build_area),
            FID_ST_CLIP_BY_BOX2D => {
                let g = from_wkb(arg_blob(&args, 0, "st_clipbybox2d")?, "st_clipbybox2d")?;
                let xmin = arg_f64(&args, 1, "st_clipbybox2d")?;
                let ymin = arg_f64(&args, 2, "st_clipbybox2d")?;
                let xmax = arg_f64(&args, 3, "st_clipbybox2d")?;
                let ymax = arg_f64(&args, 4, "st_clipbybox2d")?;
                let r = pg_proc::st_clip_by_box2d(&g, xmin, ymin, xmax, ymax)
                    .map_err(|e| format!("st_clipbybox2d: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOMETRIC_MEDIAN => g_to_geom!(args, "st_geometricmedian", pg_proc::st_geometric_median),
            FID_ST_MIN_BOUNDING_RADIUS => g_to_f64!(args, "st_minimumboundingradius", pg_proc::st_minimum_bounding_radius),
            FID_ST_MEM_UNION => g_to_geom!(args, "st_memunion", pg_proc::st_mem_union),
            FID_ST_MAX_INSCRIBED_CIRCLE => g_to_geom!(args, "st_maximuminscribedcircle", pg_proc::st_maximum_inscribed_circle),
            FID_ST_NUM_CURVES => g_to_u32!(args, "st_numcurves", pg_proc::st_num_curves),
            FID_ST_LINE_TO_CURVE => g_to_geom!(args, "st_linetocurve", pg_proc::st_line_to_curve),
            FID_ST_FORCE_CURVE => g_to_geom!(args, "st_forcecurve", pg_proc::st_force_curve),
            FID_ST_TRIANGULATE_POLYGON => g_to_geom!(args, "st_triangulatepolygon", pg_proc::st_triangulate_polygon),

            // ── v2 batch (output) ──
            FID_ST_AS_TWKB => {
                let g = from_wkb(arg_blob(&args, 0, "st_astwkb")?, "st_astwkb")?;
                let r = pg_out::st_as_twkb(&g, None)
                    .map_err(|e| format!("st_astwkb: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_AS_ENCODED_POLYLINE => {
                let g = from_wkb(arg_blob(&args, 0, "st_asencodedpolyline")?, "st_asencodedpolyline")?;
                let r = pg_out::st_as_encoded_polyline(&g, None)
                    .map_err(|e| format!("st_asencodedpolyline: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Text(r))
            }
            FID_ST_AS_LAT_LON_TEXT => {
                let g = from_wkb(arg_blob(&args, 0, "st_aslatlontext")?, "st_aslatlontext")?;
                let r = pg_out::st_as_lat_lon_text(&g, None)
                    .map_err(|e| format!("st_aslatlontext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Text(r))
            }

            // ── v2 batch (transformations) ──
            FID_ST_TRANSLATE => gff_to_geom!(args, "st_translate", pg_xform::st_translate),
            FID_ST_SCALE => gff_to_geom!(args, "st_scale", pg_xform::st_scale),
            FID_ST_TRANSSCALE => {
                let g = from_wkb(arg_blob(&args, 0, "st_transscale")?, "st_transscale")?;
                let dx = arg_f64(&args, 1, "st_transscale")?;
                let dy = arg_f64(&args, 2, "st_transscale")?;
                let sx = arg_f64(&args, 3, "st_transscale")?;
                let sy = arg_f64(&args, 4, "st_transscale")?;
                let r = pg_xform::st_transscale(&g, dx, dy, sx, sy)
                    .map_err(|e| format!("st_transscale: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_ROTATE => gd_to_geom!(args, "st_rotate", pg_xform::st_rotate),
            FID_ST_ROTATE_X => gd_to_geom!(args, "st_rotatex", pg_xform::st_rotate_x),
            FID_ST_ROTATE_Y => gd_to_geom!(args, "st_rotatey", pg_xform::st_rotate_y),
            FID_ST_ROTATE_Z => gd_to_geom!(args, "st_rotatez", pg_xform::st_rotate_z),
            FID_ST_AFFINE => {
                let g = from_wkb(arg_blob(&args, 0, "st_affine")?, "st_affine")?;
                let a = arg_f64(&args, 1, "st_affine")?;
                let b = arg_f64(&args, 2, "st_affine")?;
                let c = arg_f64(&args, 3, "st_affine")?;
                let d = arg_f64(&args, 4, "st_affine")?;
                let e = arg_f64(&args, 5, "st_affine")?;
                let f = arg_f64(&args, 6, "st_affine")?;
                let r = pg_xform::st_affine(&g, a, b, c, d, e, f)
                    .map_err(|err| format!("st_affine: {}", postgis_err_string(err)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_SWAP_ORDINATES => {
                let g = from_wkb(arg_blob(&args, 0, "st_swapordinates")?, "st_swapordinates")?;
                let o = arg_text(&args, 1, "st_swapordinates")?;
                let r = pg_xform::st_swap_ordinates(&g, o)
                    .map_err(|e| format!("st_swapordinates: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_FORCE_3DZ => g_to_geom_inf!(args, "st_force3dz", pg_xform::st_force_threedz),
            FID_ST_FORCE_3DM => g_to_geom_inf!(args, "st_force3dm", pg_xform::st_force_threedm),
            FID_ST_FORCE_4D => g_to_geom_inf!(args, "st_force4d", pg_xform::st_force_fourd),
            FID_ST_FORCE_COLLECTION => g_to_geom_inf!(args, "st_forcecollection", pg_xform::st_force_collection),
            FID_ST_SHIFT_LONGITUDE => g_to_geom!(args, "st_shiftlongitude", pg_xform::st_shift_longitude),
            FID_ST_WRAP_X => {
                let g = from_wkb(arg_blob(&args, 0, "st_wrapx")?, "st_wrapx")?;
                let w = arg_f64(&args, 1, "st_wrapx")?;
                let mv = arg_f64(&args, 2, "st_wrapx")?;
                let r = pg_xform::st_wrap_x(&g, w, mv)
                    .map_err(|e| format!("st_wrapx: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_QUANTIZE_COORDS => {
                let g = from_wkb(arg_blob(&args, 0, "st_quantizecoordinates")?, "st_quantizecoordinates")?;
                let px = arg_i64(&args, 1, "st_quantizecoordinates")? as u32;
                let py = arg_i64(&args, 2, "st_quantizecoordinates")? as u32;
                let r = pg_xform::st_quantize_coordinates(&g, px, py)
                    .map_err(|e| format!("st_quantizecoordinates: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_FORCE_SFS => g_to_geom!(args, "st_forcesfs", pg_xform::st_force_sfs),
            FID_ST_TRANSFORM => {
                let g = from_wkb(arg_blob(&args, 0, "st_transform")?, "st_transform")?;
                let to = arg_i64(&args, 1, "st_transform")? as i32;
                let r = pg_xform::st_transform(&g, to)
                    .map_err(|e| format!("st_transform: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_TRANSFORM_PIPELINE => {
                let g = from_wkb(arg_blob(&args, 0, "st_transformpipeline")?, "st_transformpipeline")?;
                let p = arg_text(&args, 1, "st_transformpipeline")?;
                let r = pg_xform::st_transform_pipeline(&g, p)
                    .map_err(|e| format!("st_transformpipeline: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_INV_TRANSFORM_PIPELINE => {
                let g = from_wkb(arg_blob(&args, 0, "st_inversetransformpipeline")?, "st_inversetransformpipeline")?;
                let p = arg_text(&args, 1, "st_inversetransformpipeline")?;
                let r = pg_xform::st_inverse_transform_pipeline(&g, p)
                    .map_err(|e| format!("st_inversetransformpipeline: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }

            // ── v2 batch (linear-ref) ──
            FID_ST_LINE_INTERPOLATE_POINT => gd_to_geom!(args, "st_lineinterpolatepoint", pg_lin::st_line_interpolate_point),
            FID_ST_LINE_INTERPOLATE_POINTS => {
                let g = from_wkb(arg_blob(&args, 0, "st_lineinterpolatepoints")?, "st_lineinterpolatepoints")?;
                let f = arg_f64(&args, 1, "st_lineinterpolatepoints")?;
                let rep = arg_i64(&args, 2, "st_lineinterpolatepoints")? != 0;
                let r = pg_lin::st_line_interpolate_points(&g, f, rep)
                    .map_err(|e| format!("st_lineinterpolatepoints: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_LINE_LOCATE_POINT => gg_to_f64!(args, "st_linelocatepoint", pg_lin::st_line_locate_point),
            FID_ST_LINE_SUBSTRING => {
                let g = from_wkb(arg_blob(&args, 0, "st_linesubstring")?, "st_linesubstring")?;
                let s = arg_f64(&args, 1, "st_linesubstring")?;
                let e = arg_f64(&args, 2, "st_linesubstring")?;
                let r = pg_lin::st_line_substring(&g, s, e)
                    .map_err(|er| format!("st_linesubstring: {}", postgis_err_string(er)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_ADD_POINT => {
                let l = from_wkb(arg_blob(&args, 0, "st_addpoint")?, "st_addpoint")?;
                let p = from_wkb(arg_blob(&args, 1, "st_addpoint")?, "st_addpoint")?;
                let r = pg_lin::st_add_point(&l, &p, None)
                    .map_err(|e| format!("st_addpoint: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_SET_POINT => {
                let l = from_wkb(arg_blob(&args, 0, "st_setpoint")?, "st_setpoint")?;
                let pos = arg_i64(&args, 1, "st_setpoint")? as u32;
                let p = from_wkb(arg_blob(&args, 2, "st_setpoint")?, "st_setpoint")?;
                let r = pg_lin::st_set_point(&l, pos, &p)
                    .map_err(|e| format!("st_setpoint: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_REMOVE_POINT => {
                let l = from_wkb(arg_blob(&args, 0, "st_removepoint")?, "st_removepoint")?;
                let pos = arg_i64(&args, 1, "st_removepoint")? as u32;
                let r = pg_lin::st_remove_point(&l, pos)
                    .map_err(|e| format!("st_removepoint: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_ADD_MEASURE => {
                let g = from_wkb(arg_blob(&args, 0, "st_addmeasure")?, "st_addmeasure")?;
                let s = arg_f64(&args, 1, "st_addmeasure")?;
                let e = arg_f64(&args, 2, "st_addmeasure")?;
                let r = pg_lin::st_add_measure(&g, s, e)
                    .map_err(|er| format!("st_addmeasure: {}", postgis_err_string(er)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_LOCATE_ALONG => gd_to_geom!(args, "st_locatealong", pg_lin::st_locate_along),
            FID_ST_LOCATE_BETWEEN => {
                let g = from_wkb(arg_blob(&args, 0, "st_locatebetween")?, "st_locatebetween")?;
                let s = arg_f64(&args, 1, "st_locatebetween")?;
                let e = arg_f64(&args, 2, "st_locatebetween")?;
                let r = pg_lin::st_locate_between(&g, s, e)
                    .map_err(|er| format!("st_locatebetween: {}", postgis_err_string(er)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_LINE_EXTEND => {
                let g = from_wkb(arg_blob(&args, 0, "st_lineextend")?, "st_lineextend")?;
                let s = arg_f64(&args, 1, "st_lineextend")?;
                let e = arg_f64(&args, 2, "st_lineextend")?;
                let r = pg_lin::st_line_extend(&g, s, e)
                    .map_err(|er| format!("st_lineextend: {}", postgis_err_string(er)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_LINE_CROSSING_DIRECTION => {
                let a = from_wkb(arg_blob(&args, 0, "st_linecrossingdirection")?, "st_linecrossingdirection")?;
                let b = from_wkb(arg_blob(&args, 1, "st_linecrossingdirection")?, "st_linecrossingdirection")?;
                let r = pg_lin::st_line_crossing_direction(&a, &b)
                    .map_err(|e| format!("st_linecrossingdirection: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Integer(r as i64))
            }
            FID_ST_LINE_INTERPOLATE_POINT_3D => gd_to_geom!(args, "st_3dlineinterpolatepoint", pg_lin::st_line_interpolate_point_threed),
            FID_ST_LOCATE_BETWEEN_ELEVATIONS => {
                let g = from_wkb(arg_blob(&args, 0, "st_locatebetweenelevations")?, "st_locatebetweenelevations")?;
                let zmin = arg_f64(&args, 1, "st_locatebetweenelevations")?;
                let zmax = arg_f64(&args, 2, "st_locatebetweenelevations")?;
                let r = pg_lin::st_locate_between_elevations(&g, zmin, zmax)
                    .map_err(|er| format!("st_locatebetweenelevations: {}", postgis_err_string(er)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }

            // ── v2 batch (three-d) ──
            FID_ST_REVERSE_3D => g_to_geom!(args, "st_3dreverse", pg_threed::st_reverse_threed),
            FID_ST_CENTROID_3D => g_to_geom!(args, "st_3dcentroid", pg_threed::st_centroid_threed),
            FID_ST_ENVELOPE_3D => g_to_geom!(args, "st_3denvelope", pg_threed::st_envelope_threed),
            FID_ST_BOUNDARY_3D => g_to_geom!(args, "st_3dboundary", pg_threed::st_boundary_threed),

            // ── v2 batch (more constructors) ──
            FID_ST_LINE_FROM_TEXT => text_to_geom!(args, "st_linefromtext", pg_ctor::st_line_from_text),
            FID_ST_POLYGON_FROM_TEXT => text_to_geom!(args, "st_polygonfromtext", pg_ctor::st_polygon_from_text),
            FID_ST_MPOINT_FROM_TEXT => text_to_geom!(args, "st_mpointfromtext", pg_ctor::st_mpoint_from_text),
            FID_ST_MLINE_FROM_TEXT => text_to_geom!(args, "st_mlinefromtext", pg_ctor::st_mline_from_text),
            FID_ST_MPOLY_FROM_TEXT => text_to_geom!(args, "st_mpolyfromtext", pg_ctor::st_mpoly_from_text),
            FID_ST_GEOMCOLL_FROM_TEXT => text_to_geom!(args, "st_geomcollfromtext", pg_ctor::st_geomcoll_from_text),
            FID_ST_GEOM_FROM_EWKB => blob_to_geom!(args, "st_geomfromewkb", pg_ctor::st_geom_from_ewkb),
            FID_ST_GEOM_FROM_HEXEWKB => text_to_geom!(args, "st_geomfromhexewkb", pg_ctor::st_geom_from_hexewkb),
            FID_ST_GEOM_FROM_GEOHASH => {
                let s = arg_text(&args, 0, "st_geomfromgeohash")?;
                let g = pg_ctor::st_geom_from_geohash(s, None)
                    .map_err(|e| format!("st_geomfromgeohash: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_POINT_FROM_GEOHASH => {
                let s = arg_text(&args, 0, "st_pointfromgeohash")?;
                let g = pg_ctor::st_point_from_geohash(s, None)
                    .map_err(|e| format!("st_pointfromgeohash: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOM_FROM_KML => text_to_geom!(args, "st_geomfromkml", pg_ctor::st_geom_from_kml),
            FID_ST_GEOM_FROM_GML => text_to_geom!(args, "st_geomfromgml", pg_ctor::st_geom_from_gml),
            FID_ST_GEOM_FROM_TWKB => blob_to_geom!(args, "st_geomfromtwkb", pg_ctor::st_geom_from_twkb),
            FID_ST_LINE_FROM_ENCODED_POLY => {
                let s = arg_text(&args, 0, "st_linefromencodedpolyline")?;
                let g = pg_ctor::st_line_from_encoded_polyline(s, None)
                    .map_err(|e| format!("st_linefromencodedpolyline: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }

            // ── Geodetic (geometry-typed) ──
            FID_ST_DISTANCE_SPHERE => gg_to_f64!(args, "st_distancesphere", pg_geog::st_distance_sphere),
            FID_ST_PROJECT => {
                let p = from_wkb(arg_blob(&args, 0, "st_project")?, "st_project")?;
                let d = arg_f64(&args, 1, "st_project")?;
                let az = arg_f64(&args, 2, "st_project")?;
                let r = pg_geog::st_project(&p, d, az)
                    .map_err(|e| format!("st_project: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }

            // ── Geography (geog crosses as BLOB of geog WKB) ──
            FID_ST_GEOGFROMTEXT => {
                let s = arg_text(&args, 0, "st_geogfromtext")?;
                let g = Geography::from_wkt(s)
                    .map_err(|e| format!("st_geogfromtext: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOGFROMWKB => {
                let b = arg_blob(&args, 0, "st_geogfromwkb")?;
                let g = geog_from_wkb(b, "st_geogfromwkb")?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_ST_GEOG_POINT => {
                let lon = arg_f64(&args, 0, "st_geogpoint")?;
                let lat = arg_f64(&args, 1, "st_geogpoint")?;
                Ok(SqlValue::Blob(Geography::point(lon, lat).as_wkb()))
            }
            FID_ST_GEOG_ASTEXT => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_astext")?, "st_geog_astext")?;
                Ok(SqlValue::Text(g.as_wkt()))
            }
            FID_ST_GEOG_DISTANCE => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_distance")?, "st_geog_distance")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_distance")?, "st_geog_distance")?;
                Ok(SqlValue::Real(pg_geog::st_geog_distance(&a, &b)))
            }
            FID_ST_GEOG_LENGTH => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_length")?, "st_geog_length")?;
                Ok(SqlValue::Real(pg_geog::st_geog_length(&g)))
            }
            FID_ST_GEOG_AREA => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_area")?, "st_geog_area")?;
                Ok(SqlValue::Real(pg_geog::st_geog_area(&g)))
            }
            FID_ST_GEOG_PERIMETER => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_perimeter")?, "st_geog_perimeter")?;
                Ok(SqlValue::Real(pg_geog::st_geog_perimeter(&g)))
            }
            FID_ST_GEOG_DWITHIN => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_dwithin")?, "st_geog_dwithin")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_dwithin")?, "st_geog_dwithin")?;
                let d = arg_f64(&args, 2, "st_geog_dwithin")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_dwithin(&a, &b, d) as i64))
            }
            FID_ST_GEOG_AZIMUTH => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_azimuth")?, "st_geog_azimuth")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_azimuth")?, "st_geog_azimuth")?;
                Ok(match pg_geog::st_geog_azimuth(&a, &b) {
                    Some(v) => SqlValue::Real(v),
                    None => SqlValue::Null,
                })
            }
            FID_ST_GEOG_PROJECT => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_project")?, "st_geog_project")?;
                let az = arg_f64(&args, 1, "st_geog_project")?;
                let d = arg_f64(&args, 2, "st_geog_project")?;
                let r = pg_geog::st_geog_project(&g, az, d)
                    .map_err(|e| format!("st_geog_project: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_SEGMENTIZE => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_segmentize")?, "st_geog_segmentize")?;
                let d = arg_f64(&args, 1, "st_geog_segmentize")?;
                let r = pg_geog::st_geog_segmentize(&g, d)
                    .map_err(|e| format!("st_geog_segmentize: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_COVERS => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_covers")?, "st_geog_covers")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_covers")?, "st_geog_covers")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_covers(&a, &b) as i64))
            }
            FID_ST_GEOG_COVERED_BY => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_coveredby")?, "st_geog_coveredby")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_coveredby")?, "st_geog_coveredby")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_covered_by(&a, &b) as i64))
            }
            FID_ST_GEOG_INTERSECTS => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_intersects")?, "st_geog_intersects")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_intersects")?, "st_geog_intersects")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_intersects(&a, &b) as i64))
            }
            FID_ST_GEOG_BUFFER => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_buffer")?, "st_geog_buffer")?;
                let d = arg_f64(&args, 1, "st_geog_buffer")?;
                let r = pg_geog::st_geog_buffer(&g, d)
                    .map_err(|e| format!("st_geog_buffer: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_BUFFER_SEGS => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_buffer_segs")?, "st_geog_buffer_segs")?;
                let d = arg_f64(&args, 1, "st_geog_buffer_segs")?;
                let qs = arg_i64(&args, 2, "st_geog_buffer_segs")? as u32;
                let r = pg_geog::st_geog_buffer_with_segs(&g, d, qs)
                    .map_err(|e| format!("st_geog_buffer_segs: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_CENTROID => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_centroid")?, "st_geog_centroid")?;
                let r = pg_geog::st_geog_centroid(&g)
                    .map_err(|e| format!("st_geog_centroid: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_INTERSECTION => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_intersection")?, "st_geog_intersection")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_intersection")?, "st_geog_intersection")?;
                let r = pg_geog::st_geog_intersection(&a, &b)
                    .map_err(|e| format!("st_geog_intersection: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_UNION => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_union")?, "st_geog_union")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_union")?, "st_geog_union")?;
                let r = pg_geog::st_geog_union(&a, &b)
                    .map_err(|e| format!("st_geog_union: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_DIFFERENCE => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_difference")?, "st_geog_difference")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_difference")?, "st_geog_difference")?;
                let r = pg_geog::st_geog_difference(&a, &b)
                    .map_err(|e| format!("st_geog_difference: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_SYM_DIFFERENCE => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_symdifference")?, "st_geog_symdifference")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_symdifference")?, "st_geog_symdifference")?;
                let r = pg_geog::st_geog_sym_difference(&a, &b)
                    .map_err(|e| format!("st_geog_symdifference: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_EXPAND => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_expand")?, "st_geog_expand")?;
                let d = arg_f64(&args, 1, "st_geog_expand")?;
                let r = pg_geog::st_geog_expand(&g, d)
                    .map_err(|e| format!("st_geog_expand: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_CLOSEST_POINT => {
                let a = geog_from_wkb(arg_blob(&args, 0, "st_geog_closestpoint")?, "st_geog_closestpoint")?;
                let b = geog_from_wkb(arg_blob(&args, 1, "st_geog_closestpoint")?, "st_geog_closestpoint")?;
                let r = pg_geog::st_geog_closest_point(&a, &b)
                    .map_err(|e| format!("st_geog_closestpoint: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_NPOINTS => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_npoints")?, "st_geog_npoints")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_npoints(&g) as i64))
            }
            FID_ST_GEOG_SUMMARY => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_summary")?, "st_geog_summary")?;
                Ok(SqlValue::Text(pg_geog::st_geog_summary(&g)))
            }
            FID_ST_GEOG_GEOMETRY_TYPE => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_geometrytype")?, "st_geog_geometrytype")?;
                Ok(SqlValue::Text(pg_geog::st_geog_geometry_type(&g)))
            }
            FID_ST_GEOG_IS_EMPTY => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_isempty")?, "st_geog_isempty")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_is_empty(&g) as i64))
            }
            FID_ST_GEOG_IS_SIMPLE => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_issimple")?, "st_geog_issimple")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_is_simple(&g) as i64))
            }
            FID_ST_GEOG_IS_CLOSED => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_isclosed")?, "st_geog_isclosed")?;
                Ok(SqlValue::Integer(pg_geog::st_geog_is_closed(&g) as i64))
            }
            FID_ST_GEOG_CONVEX_HULL => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_convexhull")?, "st_geog_convexhull")?;
                let r = pg_geog::st_geog_convex_hull(&g)
                    .map_err(|e| format!("st_geog_convexhull: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_wkb()))
            }
            FID_ST_GEOG_TO_GEOMETRY => {
                let g = geog_from_wkb(arg_blob(&args, 0, "st_geog_togeometry")?, "st_geog_togeometry")?;
                Ok(SqlValue::Blob(g.to_geometry().as_wkb()))
            }
            FID_ST_GEOMETRY_TO_GEOG => {
                let g = from_wkb(arg_blob(&args, 0, "st_togeography")?, "st_togeography")?;
                // Round-trip through WKT — geometry has no direct
                // to_geography. WGS84-style assumption.
                let wkt = g.as_wkt();
                let geog = Geography::from_wkt(&wkt)
                    .map_err(|e| format!("st_togeography: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(geog.as_wkb()))
            }

            // ── SFCGAL (postgis-sfcgal — raw WKB pass-through, no
            // Geometry resource roundtrip) ──
            FID_ST_CONVEX_HULL_3D => {
                let w = arg_blob(&args, 0, "st_3dconvexhull")?;
                let r = pg_sfcgal::st_convex_hull_threed(w)
                    .map_err(|e| format!("st_3dconvexhull: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_UNION_3D => {
                let a = arg_blob(&args, 0, "st_3dunion")?;
                let b = arg_blob(&args, 1, "st_3dunion")?;
                let r = pg_sfcgal::st_union_threed(a, b)
                    .map_err(|e| format!("st_3dunion: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_INTERSECTION_3D => {
                let a = arg_blob(&args, 0, "st_3dintersection")?;
                let b = arg_blob(&args, 1, "st_3dintersection")?;
                let r = pg_sfcgal::st_intersection_threed(a, b)
                    .map_err(|e| format!("st_3dintersection: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_DIFFERENCE_3D => {
                let a = arg_blob(&args, 0, "st_3ddifference")?;
                let b = arg_blob(&args, 1, "st_3ddifference")?;
                let r = pg_sfcgal::st_difference_threed(a, b)
                    .map_err(|e| format!("st_3ddifference: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_TESSELATE => {
                let w = arg_blob(&args, 0, "st_tesselate")?;
                let r = pg_sfcgal::st_tesselate(w)
                    .map_err(|e| format!("st_tesselate: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_STRAIGHT_SKELETON => {
                let w = arg_blob(&args, 0, "st_straightskeleton")?;
                let r = pg_sfcgal::st_straight_skeleton(w)
                    .map_err(|e| format!("st_straightskeleton: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_APPROX_MEDIAL_AXIS => {
                let w = arg_blob(&args, 0, "st_sfcgalapproximatemedialaxis")?;
                let r = pg_sfcgal::st_approximate_medial_axis(w)
                    .map_err(|e| format!("st_sfcgalapproximatemedialaxis: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_EXTRUDE => {
                let w = arg_blob(&args, 0, "st_extrude")?;
                let dx = arg_f64(&args, 1, "st_extrude")?;
                let dy = arg_f64(&args, 2, "st_extrude")?;
                let dz = arg_f64(&args, 3, "st_extrude")?;
                let r = pg_sfcgal::st_extrude(w, dx, dy, dz)
                    .map_err(|e| format!("st_extrude: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_MINKOWSKI_SUM => {
                let a = arg_blob(&args, 0, "st_minkowskisum")?;
                let b = arg_blob(&args, 1, "st_minkowskisum")?;
                let r = pg_sfcgal::st_minkowski_sum(a, b)
                    .map_err(|e| format!("st_minkowskisum: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_VOLUME => {
                let w = arg_blob(&args, 0, "st_volume")?;
                Ok(SqlValue::Real(pg_sfcgal::st_volume(w)))
            }
            FID_ST_AREA_3D => {
                let w = arg_blob(&args, 0, "st_3darea")?;
                Ok(SqlValue::Real(pg_sfcgal::st_area_threed(w)))
            }
            FID_ST_DISTANCE_3D_SFCGAL => {
                let a = arg_blob(&args, 0, "st_sfcgaldistance3d")?;
                let b = arg_blob(&args, 1, "st_sfcgaldistance3d")?;
                Ok(SqlValue::Real(pg_sfcgal::st_distance_threed(a, b)))
            }
            FID_ST_TRANSLATE_3D => {
                let w = arg_blob(&args, 0, "st_3dtranslate")?;
                let dx = arg_f64(&args, 1, "st_3dtranslate")?;
                let dy = arg_f64(&args, 2, "st_3dtranslate")?;
                let dz = arg_f64(&args, 3, "st_3dtranslate")?;
                let r = pg_sfcgal::st_translate_threed(w, dx, dy, dz)
                    .map_err(|e| format!("st_3dtranslate: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_SCALE_3D => {
                let w = arg_blob(&args, 0, "st_3dscale")?;
                let sx = arg_f64(&args, 1, "st_3dscale")?;
                let sy = arg_f64(&args, 2, "st_3dscale")?;
                let sz = arg_f64(&args, 3, "st_3dscale")?;
                let r = pg_sfcgal::st_scale_threed(w, sx, sy, sz)
                    .map_err(|e| format!("st_3dscale: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }
            FID_ST_ROTATE_3D => {
                let w = arg_blob(&args, 0, "st_3drotate")?;
                let angle = arg_f64(&args, 1, "st_3drotate")?;
                let ax = arg_f64(&args, 2, "st_3drotate")?;
                let ay = arg_f64(&args, 3, "st_3drotate")?;
                let az = arg_f64(&args, 4, "st_3drotate")?;
                let r = pg_sfcgal::st_rotate_threed(w, angle, ax, ay, az)
                    .map_err(|e| format!("st_3drotate: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(r))
            }

            // ── Direct sfcgal-wasm (handle-keyed) ──
            FID_SFC_VERSION => {
                // world-level fns from sfcgal-world (`version`,
                // `full-version`) aren't importable through an
                // interface boundary; report a stub instead.
                Ok(SqlValue::Text("sfcgal (direct compose)".into()))
            }
            FID_SFC_AS_STL => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_asstl")?, "st_sfc_asstl")?;
                let s = sfc_string(sf_io::write_stl(h), "st_sfc_asstl");
                sf_geom::destroy(h);
                Ok(SqlValue::Text(s?))
            }
            FID_SFC_AS_STL_BINARY => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_asstlbinary")?, "st_sfc_asstlbinary")?;
                let bytes = sf_io::write_stl_binary(h);
                sf_geom::destroy(h);
                Ok(SqlValue::Blob(bytes))
            }
            FID_SFC_AS_OBJ => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_asobj")?, "st_sfc_asobj")?;
                let s = sfc_string(sf_io::write_obj(h), "st_sfc_asobj");
                sf_geom::destroy(h);
                Ok(SqlValue::Text(s?))
            }
            FID_SFC_AS_VTK => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_asvtk")?, "st_sfc_asvtk")?;
                let s = sfc_string(sf_io::write_vtk(h), "st_sfc_asvtk");
                sf_geom::destroy(h);
                Ok(SqlValue::Text(s?))
            }
            FID_SFC_ALPHA_SHAPE => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_alphashape")?, "st_sfc_alphashape")?;
                let a = arg_f64(&args, 1, "st_sfc_alphashape")?;
                match sfc_geom(sf_geom::alpha_shape(h, a), "st_sfc_alphashape") {
                    Ok(r) => {
                        sf_geom::destroy(h);
                        Ok(SqlValue::Blob(sfc_take_wkb(r)))
                    }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_OPTIMAL_ALPHA_SHAPE => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_optimalalphashape")?, "st_sfc_optimalalphashape")?;
                match sfc_geom(sf_geom::optimal_alpha_shape(h), "st_sfc_optimalalphashape") {
                    Ok(r) => { sf_geom::destroy(h); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_EXTRUDE_STRAIGHT => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_extrudestraight")?, "st_sfc_extrudestraight")?;
                let height = arg_f64(&args, 1, "st_sfc_extrudestraight")?;
                match sfc_geom(sf_geom::extrude_straight(h, height), "st_sfc_extrudestraight") {
                    Ok(r) => { sf_geom::destroy(h); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_EXTRUDE_STRAIGHT_SKELETON => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_extrudestraightskeleton")?, "st_sfc_extrudestraightskeleton")?;
                let height = arg_f64(&args, 1, "st_sfc_extrudestraightskeleton")?;
                match sfc_geom(sf_geom::extrude_straight_skeleton(h, height), "st_sfc_extrudestraightskeleton") {
                    Ok(r) => { sf_geom::destroy(h); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_MAKE_VALID => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_makevalid")?, "st_sfc_makevalid")?;
                match sfc_geom(sf_geom::make_valid(h), "st_sfc_makevalid") {
                    Ok(r) => { sf_geom::destroy(h); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_IS_VALID => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_isvalid")?, "st_sfc_isvalid")?;
                let v = sf_geom::is_valid(h);
                sf_geom::destroy(h);
                Ok(SqlValue::Integer(v as i64))
            }
            FID_SFC_AREA => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_area")?, "st_sfc_area")?;
                let v = sfc_f64(sf_geom::area(h), "st_sfc_area");
                sf_geom::destroy(h);
                Ok(SqlValue::Real(v?))
            }
            FID_SFC_VOLUME => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_volume")?, "st_sfc_volume")?;
                let v = sfc_f64(sf_geom::volume(h), "st_sfc_volume");
                sf_geom::destroy(h);
                Ok(SqlValue::Real(v?))
            }
            FID_SFC_LENGTH => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_length")?, "st_sfc_length")?;
                let v = sfc_f64(sf_geom::length(h), "st_sfc_length");
                sf_geom::destroy(h);
                Ok(SqlValue::Real(v?))
            }
            FID_SFC_DISTANCE => {
                let a = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_distance")?, "st_sfc_distance")?;
                let b = sfc_read_wkb(arg_blob(&args, 1, "st_sfc_distance")?, "st_sfc_distance")?;
                let v = sfc_f64(sf_geom::distance(a, b), "st_sfc_distance");
                sf_geom::destroy(a);
                sf_geom::destroy(b);
                Ok(SqlValue::Real(v?))
            }
            FID_SFC_TRIANGLE => {
                sfc_ensure_init();
                let p1 = sf_geom::Coordinate::Coord2d(sf_geom::Coordinate2d {
                    x: arg_f64(&args, 0, "st_sfc_triangle")?,
                    y: arg_f64(&args, 1, "st_sfc_triangle")?,
                });
                let p2 = sf_geom::Coordinate::Coord2d(sf_geom::Coordinate2d {
                    x: arg_f64(&args, 2, "st_sfc_triangle")?,
                    y: arg_f64(&args, 3, "st_sfc_triangle")?,
                });
                let p3 = sf_geom::Coordinate::Coord2d(sf_geom::Coordinate2d {
                    x: arg_f64(&args, 4, "st_sfc_triangle")?,
                    y: arg_f64(&args, 5, "st_sfc_triangle")?,
                });
                let h = sfc_geom(sf_geom::triangle(p1, p2, p3), "st_sfc_triangle")?;
                Ok(SqlValue::Blob(sfc_take_wkb(h)))
            }
            FID_SFC_TESSELLATE => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_tessellate")?, "st_sfc_tessellate")?;
                match sfc_geom(sf_geom::tessellate(h), "st_sfc_tessellate") {
                    Ok(r) => { sf_geom::destroy(h); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_CONVEX_HULL => {
                let h = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_convexhull")?, "st_sfc_convexhull")?;
                match sfc_geom(sf_geom::convex_hull(h), "st_sfc_convexhull") {
                    Ok(r) => { sf_geom::destroy(h); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(h); Err(e) }
                }
            }
            FID_SFC_DIFFERENCE => {
                let a = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_difference")?, "st_sfc_difference")?;
                let b = sfc_read_wkb(arg_blob(&args, 1, "st_sfc_difference")?, "st_sfc_difference")?;
                match sfc_geom(sf_geom::difference(a, b), "st_sfc_difference") {
                    Ok(r) => { sf_geom::destroy(a); sf_geom::destroy(b); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(a); sf_geom::destroy(b); Err(e) }
                }
            }
            FID_SFC_INTERSECTION => {
                let a = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_intersection")?, "st_sfc_intersection")?;
                let b = sfc_read_wkb(arg_blob(&args, 1, "st_sfc_intersection")?, "st_sfc_intersection")?;
                match sfc_geom(sf_geom::intersection(a, b), "st_sfc_intersection") {
                    Ok(r) => { sf_geom::destroy(a); sf_geom::destroy(b); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(a); sf_geom::destroy(b); Err(e) }
                }
            }
            FID_SFC_UNION => {
                let a = sfc_read_wkb(arg_blob(&args, 0, "st_sfc_union")?, "st_sfc_union")?;
                let b = sfc_read_wkb(arg_blob(&args, 1, "st_sfc_union")?, "st_sfc_union")?;
                match sfc_geom(sf_geom::union(a, b), "st_sfc_union") {
                    Ok(r) => { sf_geom::destroy(a); sf_geom::destroy(b); Ok(SqlValue::Blob(sfc_take_wkb(r))) }
                    Err(e) => { sf_geom::destroy(a); sf_geom::destroy(b); Err(e) }
                }
            }

            // ── Raster ──
            FID_RST_WIDTH => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_width")?, "st_rast_width")?;
                Ok(SqlValue::Integer(pg_rast_acc::st_width(&r) as i64))
            }
            FID_RST_HEIGHT => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_height")?, "st_rast_height")?;
                Ok(SqlValue::Integer(pg_rast_acc::st_height(&r) as i64))
            }
            FID_RST_NUM_BANDS => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_numbands")?, "st_rast_numbands")?;
                Ok(SqlValue::Integer(pg_rast_acc::st_num_bands(&r) as i64))
            }
            FID_RST_UPPER_LEFT_X => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_upperleftx")?, "st_rast_upperleftx")?;
                Ok(SqlValue::Real(pg_rast_acc::st_upper_left_x(&r)))
            }
            FID_RST_UPPER_LEFT_Y => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_upperlefty")?, "st_rast_upperlefty")?;
                Ok(SqlValue::Real(pg_rast_acc::st_upper_left_y(&r)))
            }
            FID_RST_SCALE_X => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_scalex")?, "st_rast_scalex")?;
                Ok(SqlValue::Real(pg_rast_acc::st_scale_x(&r)))
            }
            FID_RST_SCALE_Y => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_scaley")?, "st_rast_scaley")?;
                Ok(SqlValue::Real(pg_rast_acc::st_scale_y(&r)))
            }
            FID_RST_SKEW_X => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_skewx")?, "st_rast_skewx")?;
                Ok(SqlValue::Real(pg_rast_acc::st_skew_x(&r)))
            }
            FID_RST_SKEW_Y => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_skewy")?, "st_rast_skewy")?;
                Ok(SqlValue::Real(pg_rast_acc::st_skew_y(&r)))
            }
            FID_RST_SRID => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_srid")?, "st_rast_srid")?;
                Ok(SqlValue::Integer(pg_rast_acc::st_srid(&r) as i64))
            }
            FID_RST_HAS_NO_BAND => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_hasnoband")?, "st_rast_hasnoband")?;
                let b = arg_i64(&args, 1, "st_rast_hasnoband")? as u32;
                Ok(SqlValue::Integer(pg_rast_acc::st_has_no_band(&r, b) as i64))
            }
            FID_RST_VALUE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_value")?, "st_rast_value")?;
                let band = arg_i64(&args, 1, "st_rast_value")? as u32;
                let x = arg_i64(&args, 2, "st_rast_value")? as u32;
                let y = arg_i64(&args, 3, "st_rast_value")? as u32;
                let v = pg_rast_acc::st_value(&r, band, x, y)
                    .map_err(|e| format!("st_rast_value: {}", raster_err_string(e)))?;
                Ok(SqlValue::Real(v))
            }
            FID_RST_NEAREST_VALUE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_nearestvalue")?, "st_rast_nearestvalue")?;
                let band = arg_i64(&args, 1, "st_rast_nearestvalue")? as u32;
                let x = arg_i64(&args, 2, "st_rast_nearestvalue")? as u32;
                let y = arg_i64(&args, 3, "st_rast_nearestvalue")? as u32;
                let v = pg_rast_px::st_nearest_value(&r, band, x, y)
                    .map_err(|e| format!("st_rast_nearestvalue: {}", raster_err_string(e)))?;
                Ok(SqlValue::Real(v))
            }
            FID_RST_PIXEL_AS_POINT => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_pixelaspoint")?, "st_rast_pixelaspoint")?;
                let x = arg_i64(&args, 1, "st_rast_pixelaspoint")? as u32;
                let y = arg_i64(&args, 2, "st_rast_pixelaspoint")? as u32;
                let g = pg_rast_acc::st_pixel_as_point(&r, x, y)
                    .map_err(|e| format!("st_rast_pixelaspoint: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_RST_PIXEL_AS_POLYGON => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_pixelaspolygon")?, "st_rast_pixelaspolygon")?;
                let x = arg_i64(&args, 1, "st_rast_pixelaspolygon")? as u32;
                let y = arg_i64(&args, 2, "st_rast_pixelaspolygon")? as u32;
                let g = pg_rast_px::st_pixel_as_polygon(&r, x, y)
                    .map_err(|e| format!("st_rast_pixelaspolygon: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_RST_PIXEL_AS_CENTROID => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_pixelascentroid")?, "st_rast_pixelascentroid")?;
                let x = arg_i64(&args, 1, "st_rast_pixelascentroid")? as u32;
                let y = arg_i64(&args, 2, "st_rast_pixelascentroid")? as u32;
                let g = pg_rast_px::st_pixel_as_centroid(&r, x, y)
                    .map_err(|e| format!("st_rast_pixelascentroid: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_RST_RASTER_TO_WORLD_COORD_X => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_rastertoworldcoordx")?, "st_rast_rastertoworldcoordx")?;
                let col = arg_i64(&args, 1, "st_rast_rastertoworldcoordx")? as u32;
                let row = arg_i64(&args, 2, "st_rast_rastertoworldcoordx")? as u32;
                Ok(SqlValue::Real(pg_rast_px::st_raster_to_world_coord_x(&r, col, row)))
            }
            FID_RST_RASTER_TO_WORLD_COORD_Y => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_rastertoworldcoordy")?, "st_rast_rastertoworldcoordy")?;
                let col = arg_i64(&args, 1, "st_rast_rastertoworldcoordy")? as u32;
                let row = arg_i64(&args, 2, "st_rast_rastertoworldcoordy")? as u32;
                Ok(SqlValue::Real(pg_rast_px::st_raster_to_world_coord_y(&r, col, row)))
            }
            FID_RST_AS_PNG => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_aspng")?, "st_rast_aspng")?;
                let band = arg_i64(&args, 1, "st_rast_aspng")? as u32;
                let bytes = pg_rast_out::st_as_png(&r, band)
                    .map_err(|e| format!("st_rast_aspng: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(bytes))
            }
            FID_RST_AS_TIFF => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_astiff")?, "st_rast_astiff")?;
                let bytes = pg_rast_out::st_as_tiff(&r)
                    .map_err(|e| format!("st_rast_astiff: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(bytes))
            }
            FID_RST_R_INTERSECTS => {
                let a = rast_from_blob(arg_blob(&args, 0, "st_rast_intersects")?, "st_rast_intersects")?;
                let b = rast_from_blob(arg_blob(&args, 1, "st_rast_intersects")?, "st_rast_intersects")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_intersects(&a, &b) as i64))
            }
            FID_RST_R_CONTAINS => {
                let a = rast_from_blob(arg_blob(&args, 0, "st_rast_contains")?, "st_rast_contains")?;
                let b = rast_from_blob(arg_blob(&args, 1, "st_rast_contains")?, "st_rast_contains")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_contains(&a, &b) as i64))
            }
            FID_RST_R_WITHIN => {
                let a = rast_from_blob(arg_blob(&args, 0, "st_rast_within")?, "st_rast_within")?;
                let b = rast_from_blob(arg_blob(&args, 1, "st_rast_within")?, "st_rast_within")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_within(&a, &b) as i64))
            }
            FID_RST_R_COVERS => {
                let a = rast_from_blob(arg_blob(&args, 0, "st_rast_covers")?, "st_rast_covers")?;
                let b = rast_from_blob(arg_blob(&args, 1, "st_rast_covers")?, "st_rast_covers")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_covers(&a, &b) as i64))
            }
            FID_RST_R_OVERLAPS => {
                let a = rast_from_blob(arg_blob(&args, 0, "st_rast_overlaps")?, "st_rast_overlaps")?;
                let b = rast_from_blob(arg_blob(&args, 1, "st_rast_overlaps")?, "st_rast_overlaps")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_overlaps(&a, &b) as i64))
            }
            FID_RST_R_INTERSECTS_GEOM => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_intersectsgeom")?, "st_rast_intersectsgeom")?;
                let g = from_wkb(arg_blob(&args, 1, "st_rast_intersectsgeom")?, "st_rast_intersectsgeom")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_intersects_geom(&r, &g) as i64))
            }
            FID_RST_R_CONTAINS_GEOM => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_containsgeom")?, "st_rast_containsgeom")?;
                let g = from_wkb(arg_blob(&args, 1, "st_rast_containsgeom")?, "st_rast_containsgeom")?;
                Ok(SqlValue::Integer(pg_rast_pred::st_raster_contains_geom(&r, &g) as i64))
            }
            FID_RST_POLYGON_FROM_RAST => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_polygon")?, "st_rast_polygon")?;
                let band = arg_i64(&args, 1, "st_rast_polygon")? as u32;
                let g = pg_rast_vec::st_polygon(&r, band)
                    .map_err(|e| format!("st_rast_polygon: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_RST_CONVEX_HULL => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_convexhull")?, "st_rast_convexhull")?;
                let g = pg_rast_pred::st_raster_convex_hull(&r)
                    .map_err(|e| format!("st_rast_convexhull: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            FID_RST_SLOPE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_slope")?, "st_rast_slope")?;
                let band = arg_i64(&args, 1, "st_rast_slope")? as u32;
                let out = pg_rast_proc::st_slope(&r, band)
                    .map_err(|e| format!("st_rast_slope: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_ASPECT => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_aspect")?, "st_rast_aspect")?;
                let band = arg_i64(&args, 1, "st_rast_aspect")? as u32;
                let out = pg_rast_proc::st_aspect(&r, band)
                    .map_err(|e| format!("st_rast_aspect: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_ROUGHNESS => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_roughness")?, "st_rast_roughness")?;
                let band = arg_i64(&args, 1, "st_rast_roughness")? as u32;
                let out = pg_rast_proc::st_roughness(&r, band)
                    .map_err(|e| format!("st_rast_roughness: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_TRI => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_tri")?, "st_rast_tri")?;
                let band = arg_i64(&args, 1, "st_rast_tri")? as u32;
                let out = pg_rast_proc::st_tri(&r, band)
                    .map_err(|e| format!("st_rast_tri: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_TPI => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_tpi")?, "st_rast_tpi")?;
                let band = arg_i64(&args, 1, "st_rast_tpi")? as u32;
                let out = pg_rast_proc::st_tpi(&r, band)
                    .map_err(|e| format!("st_rast_tpi: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }

            // ── Raster v2 ──
            FID_RST_MAKE_EMPTY => {
                let w = arg_i64(&args, 0, "st_rast_makeemptyraster")? as u32;
                let h = arg_i64(&args, 1, "st_rast_makeemptyraster")? as u32;
                let ulx = arg_f64(&args, 2, "st_rast_makeemptyraster")?;
                let uly = arg_f64(&args, 3, "st_rast_makeemptyraster")?;
                let sx = arg_f64(&args, 4, "st_rast_makeemptyraster")?;
                let sy = arg_f64(&args, 5, "st_rast_makeemptyraster")?;
                let skx = arg_f64(&args, 6, "st_rast_makeemptyraster")?;
                let sky = arg_f64(&args, 7, "st_rast_makeemptyraster")?;
                let srid = arg_i64(&args, 8, "st_rast_makeemptyraster")? as i32;
                let r = pg_rast_ctor::st_make_empty_raster(w, h, ulx, uly, sx, sy, skx, sky, srid)
                    .map_err(|e| format!("st_rast_makeemptyraster: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(r.as_binary()))
            }
            FID_RST_ADD_BAND => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_addband")?, "st_rast_addband")?;
                let ptype = parse_pixel_type(arg_text(&args, 1, "st_rast_addband")?)?;
                let init = arg_f64(&args, 2, "st_rast_addband")?;
                let nodata = match args.get(3) {
                    Some(SqlValue::Null) | None => None,
                    Some(_) => Some(arg_f64(&args, 3, "st_rast_addband")?),
                };
                let out = pg_rast_ctor::st_add_band(&r, ptype, init, nodata)
                    .map_err(|e| format!("st_rast_addband: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_SET_VALUE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_setvalue")?, "st_rast_setvalue")?;
                let band = arg_i64(&args, 1, "st_rast_setvalue")? as u32;
                let x = arg_i64(&args, 2, "st_rast_setvalue")? as u32;
                let y = arg_i64(&args, 3, "st_rast_setvalue")? as u32;
                let v = arg_f64(&args, 4, "st_rast_setvalue")?;
                let out = pg_rast_acc::st_set_value(&r, band, x, y, v)
                    .map_err(|e| format!("st_rast_setvalue: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }

            // Summary stats decomposed  one scalar per field. Each
            // call hits summary_stats fresh; SQL users can JOIN them
            // if they want all fields without paying the cost twice.
            FID_RST_SUMMARY_COUNT
            | FID_RST_SUMMARY_SUM
            | FID_RST_SUMMARY_MEAN
            | FID_RST_SUMMARY_STDDEV
            | FID_RST_SUMMARY_MIN
            | FID_RST_SUMMARY_MAX => {
                let name = match func_id {
                    FID_RST_SUMMARY_COUNT => "st_rast_count",
                    FID_RST_SUMMARY_SUM => "st_rast_sum",
                    FID_RST_SUMMARY_MEAN => "st_rast_mean",
                    FID_RST_SUMMARY_STDDEV => "st_rast_stddev",
                    FID_RST_SUMMARY_MIN => "st_rast_min",
                    _ => "st_rast_max",
                };
                let r = rast_from_blob(arg_blob(&args, 0, name)?, name)?;
                let band = arg_i64(&args, 1, name)? as u32;
                let s = pg_rast_stats::st_summary_stats(&r, band)
                    .map_err(|e| format!("{name}: {}", raster_err_string(e)))?;
                Ok(match func_id {
                    FID_RST_SUMMARY_COUNT => SqlValue::Integer(s.count as i64),
                    FID_RST_SUMMARY_SUM => SqlValue::Real(s.sum),
                    FID_RST_SUMMARY_MEAN => SqlValue::Real(s.mean),
                    FID_RST_SUMMARY_STDDEV => SqlValue::Real(s.stddev),
                    FID_RST_SUMMARY_MIN => SqlValue::Real(s.min),
                    _ => SqlValue::Real(s.max),
                })
            }
            FID_RST_QUANTILE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_quantile")?, "st_rast_quantile")?;
                let band = arg_i64(&args, 1, "st_rast_quantile")? as u32;
                let q = arg_f64(&args, 2, "st_rast_quantile")?;
                let v = pg_rast_stats::st_quantile(&r, band, q)
                    .map_err(|e| format!("st_rast_quantile: {}", raster_err_string(e)))?;
                Ok(SqlValue::Real(v))
            }
            FID_RST_WORLD_TO_RAST_X => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_worldtorastercoordx")?, "st_rast_worldtorastercoordx")?;
                let wx = arg_f64(&args, 1, "st_rast_worldtorastercoordx")?;
                let wy = arg_f64(&args, 2, "st_rast_worldtorastercoordx")?;
                let (col, _row) = pg_rast_px::st_world_to_raster_coord(&r, wx, wy);
                Ok(SqlValue::Integer(col as i64))
            }
            FID_RST_WORLD_TO_RAST_Y => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_worldtorastercoordy")?, "st_rast_worldtorastercoordy")?;
                let wx = arg_f64(&args, 1, "st_rast_worldtorastercoordy")?;
                let wy = arg_f64(&args, 2, "st_rast_worldtorastercoordy")?;
                let (_col, row) = pg_rast_px::st_world_to_raster_coord(&r, wx, wy);
                Ok(SqlValue::Integer(row as i64))
            }
            FID_RST_HILL_SHADE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_hillshade")?, "st_rast_hillshade")?;
                let band = arg_i64(&args, 1, "st_rast_hillshade")? as u32;
                let az = arg_f64(&args, 2, "st_rast_hillshade")?;
                let alt = arg_f64(&args, 3, "st_rast_hillshade")?;
                let out = pg_rast_proc::st_hill_shade(&r, band, az, alt)
                    .map_err(|e| format!("st_rast_hillshade: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_RESIZE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_resize")?, "st_rast_resize")?;
                let w = arg_i64(&args, 1, "st_rast_resize")? as u32;
                let h = arg_i64(&args, 2, "st_rast_resize")? as u32;
                let alg = arg_text(&args, 3, "st_rast_resize")?;
                let out = pg_rast_proc::st_resize(&r, w, h, alg)
                    .map_err(|e| format!("st_rast_resize: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_RESCALE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_rescale")?, "st_rast_rescale")?;
                let sx = arg_f64(&args, 1, "st_rast_rescale")?;
                let sy = arg_f64(&args, 2, "st_rast_rescale")?;
                let alg = arg_text(&args, 3, "st_rast_rescale")?;
                let out = pg_rast_proc::st_rescale(&r, sx, sy, alg)
                    .map_err(|e| format!("st_rast_rescale: {}", raster_err_string(e)))?;
                Ok(SqlValue::Blob(out.as_binary()))
            }
            FID_RST_BAND_PIXEL_TYPE => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_bandpixeltype")?, "st_rast_bandpixeltype")?;
                let band = arg_i64(&args, 1, "st_rast_bandpixeltype")? as u32;
                let p = pg_rast_acc::st_band_pixel_type(&r, band)
                    .map_err(|e| format!("st_rast_bandpixeltype: {}", raster_err_string(e)))?;
                Ok(SqlValue::Text(pixel_type_str(p).to_string()))
            }
            FID_RST_BAND_NODATA => {
                let r = rast_from_blob(arg_blob(&args, 0, "st_rast_bandnodatavalue")?, "st_rast_bandnodatavalue")?;
                let band = arg_i64(&args, 1, "st_rast_bandnodatavalue")? as u32;
                let v = pg_rast_acc::st_band_nodata_value(&r, band)
                    .map_err(|e| format!("st_rast_bandnodatavalue: {}", raster_err_string(e)))?;
                Ok(match v {
                    Some(x) => SqlValue::Real(x),
                    None => SqlValue::Null,
                })
            }

            // ── Topology ──
            FID_TOPO_NAME => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_name")?, "st_topo_name")?;
                Ok(SqlValue::Text(t.name()))
            }
            FID_TOPO_SRID => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_srid")?, "st_topo_srid")?;
                Ok(SqlValue::Integer(t.srid() as i64))
            }
            FID_TOPO_PRECISION => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_precision")?, "st_topo_precision")?;
                Ok(SqlValue::Real(t.precision()))
            }
            FID_TOPO_NODE_COUNT => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_nodecount")?, "st_topo_nodecount")?;
                Ok(SqlValue::Integer(t.node_count() as i64))
            }
            FID_TOPO_EDGE_COUNT => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_edgecount")?, "st_topo_edgecount")?;
                Ok(SqlValue::Integer(t.edge_count() as i64))
            }
            FID_TOPO_FACE_COUNT => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_facecount")?, "st_topo_facecount")?;
                Ok(SqlValue::Integer(t.face_count() as i64))
            }
            FID_TOPO_AS_TOPOJSON => {
                let t = topo_from_bytes(arg_blob(&args, 0, "st_topo_astopojson")?, "st_topo_astopojson")?;
                Ok(SqlValue::Text(pg_topo_out::as_topojson(&t)))
            }

            // ── Spatial-index (STRtree) ──
            FID_STRTREE_CREATE => {
                let cap = arg_i64(&args, 0, "st_strtree_create")? as u32;
                let h = pg_strtree::create_index(cap);
                Ok(SqlValue::Integer(h as i64))
            }
            FID_STRTREE_INSERT => {
                let h = arg_i64(&args, 0, "st_strtree_insert")? as u64;
                let wkb = arg_blob(&args, 1, "st_strtree_insert")?;
                let id = arg_i64(&args, 2, "st_strtree_insert")? as u64;
                let ok = pg_strtree::insert_wkb(h, wkb, id);
                Ok(SqlValue::Integer(ok as i64))
            }
            FID_STRTREE_BUILD => {
                let h = arg_i64(&args, 0, "st_strtree_build")? as u64;
                let ok = pg_strtree::build(h);
                Ok(SqlValue::Integer(ok as i64))
            }
            FID_STRTREE_QUERY => {
                let h = arg_i64(&args, 0, "st_strtree_query")? as u64;
                let minx = arg_f64(&args, 1, "st_strtree_query")?;
                let miny = arg_f64(&args, 2, "st_strtree_query")?;
                let maxx = arg_f64(&args, 3, "st_strtree_query")?;
                let maxy = arg_f64(&args, 4, "st_strtree_query")?;
                let ids = pg_strtree::query_envelope(h, minx, miny, maxx, maxy);
                let joined: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
                Ok(SqlValue::Text(format!("[{}]", joined.join(","))))
            }
            FID_STRTREE_NEAREST => {
                let h = arg_i64(&args, 0, "st_strtree_nearest")? as u64;
                let wkb = arg_blob(&args, 1, "st_strtree_nearest")?;
                Ok(SqlValue::Integer(pg_strtree::nearest(h, wkb) as i64))
            }
            FID_STRTREE_KNN => {
                let h = arg_i64(&args, 0, "st_strtree_knn")? as u64;
                let wkb = arg_blob(&args, 1, "st_strtree_knn")?;
                let k = arg_i64(&args, 2, "st_strtree_knn")? as u32;
                let ids = pg_strtree::query_knn(h, wkb, k);
                let joined: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
                Ok(SqlValue::Text(format!("[{}]", joined.join(","))))
            }
            FID_STRTREE_WITHIN => {
                let h = arg_i64(&args, 0, "st_strtree_within")? as u64;
                let wkb = arg_blob(&args, 1, "st_strtree_within")?;
                let dist = arg_f64(&args, 2, "st_strtree_within")?;
                let ids = pg_strtree::query_within_distance(h, wkb, dist);
                let joined: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
                Ok(SqlValue::Text(format!("[{}]", joined.join(","))))
            }
            FID_STRTREE_DESTROY => {
                let h = arg_i64(&args, 0, "st_strtree_destroy")? as u64;
                pg_strtree::destroy_index(h);
                Ok(SqlValue::Integer(1))
            }

            other => Err(format!("postgis bridge: unknown func id {other}")),
        }
    }
}

// ───────────── Aggregate dispatch ─────────────

impl AggregateGuest for PostgisBridge {
    fn step(
        func_id: u64,
        context_id: u64,
        args: Vec<SqlValue>,
    ) -> Result<(), String> {
        // NULL arg = no-op (SQL aggregate convention).
        let arg0 = match args.first() {
            Some(SqlValue::Null) | None => return Ok(()),
            Some(v) => v,
        };
        let bytes = match arg0 {
            SqlValue::Blob(b) => b.clone(),
            SqlValue::Text(s) => s.as_bytes().to_vec(),
            _ => return Err("postgis agg: arg 0 must be BLOB (WKB)".to_string()),
        };
        AGGS.with(|m| {
            let mut tbl = m.borrow_mut();
            let entry = tbl.entry(context_id).or_default();
            entry.wkbs.push(bytes);
            match func_id {
                AGG_ST_CLUSTER_WITHIN => {
                    if entry.distance.is_none() {
                        if let Some(v) = arg_to_f64(args.get(1)) {
                            entry.distance = Some(v);
                        }
                    }
                }
                AGG_ST_CLUSTER_DBSCAN => {
                    if entry.eps.is_none() {
                        if let Some(v) = arg_to_f64(args.get(1)) {
                            entry.eps = Some(v);
                        }
                    }
                    if entry.min_points.is_none() {
                        if let Some(v) = arg_to_i64(args.get(2)) {
                            entry.min_points = Some(v as u32);
                        }
                    }
                }
                AGG_ST_CLUSTER_KMEANS => {
                    if entry.k.is_none() {
                        if let Some(v) = arg_to_i64(args.get(1)) {
                            entry.k = Some(v as u32);
                        }
                    }
                }
                _ => {}
            }
        });
        Ok(())
    }

    fn finalize(func_id: u64, context_id: u64) -> Result<SqlValue, String> {
        let state = AGGS.with(|m| m.borrow_mut().remove(&context_id));
        let state = match state {
            Some(s) => s,
            None => return Ok(SqlValue::Null),
        };
        if state.wkbs.is_empty() {
            return Ok(SqlValue::Null);
        }
        // Reconstitute geometries.
        let mut geoms = Vec::with_capacity(state.wkbs.len());
        for (i, wkb) in state.wkbs.iter().enumerate() {
            geoms.push(
                Geometry::from_wkb(wkb)
                    .map_err(|e| format!("agg arg {i}: {}", postgis_err_string(e)))?,
            );
        }
        let refs: Vec<&Geometry> = geoms.iter().collect();
        match func_id {
            AGG_ST_UNION => {
                let g = pg_agg::st_union_aggregate(&refs)
                    .map_err(|e| format!("st_union_agg: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            AGG_ST_POLYGONIZE => {
                let g = pg_agg::st_polygonize_aggregate(&refs)
                    .map_err(|e| format!("st_polygonize_agg: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            AGG_ST_MAKELINE => {
                let g = pg_agg::st_make_line_aggregate(&refs)
                    .map_err(|e| format!("st_makeline_agg: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(g.as_wkb()))
            }
            AGG_ST_CLUSTER_INTERSECTING => {
                let parts = pg_agg::st_cluster_intersecting_aggregate(&refs)
                    .map_err(|e| {
                        format!("st_clusterintersecting_agg: {}", postgis_err_string(e))
                    })?;
                // SQL aggregates return a single value  collapse
                // the cluster list into one GeometryCollection.
                let part_refs: Vec<&Geometry> = parts.iter().collect();
                let collected = pg_acc::st_collect(&part_refs).map_err(|e| {
                    format!("st_clusterintersecting_agg: {}", postgis_err_string(e))
                })?;
                Ok(SqlValue::Blob(collected.as_wkb()))
            }
            AGG_ST_CLUSTER_WITHIN => {
                let distance = state
                    .distance
                    .ok_or_else(|| "st_clusterwithin_agg: distance never seen".to_string())?;
                let parts = pg_agg::st_cluster_within_aggregate(&refs, distance)
                    .map_err(|e| format!("st_clusterwithin_agg: {}", postgis_err_string(e)))?;
                let part_refs: Vec<&Geometry> = parts.iter().collect();
                let collected = pg_acc::st_collect(&part_refs)
                    .map_err(|e| format!("st_clusterwithin_agg: {}", postgis_err_string(e)))?;
                Ok(SqlValue::Blob(collected.as_wkb()))
            }
            AGG_ST_EXTENT_3D => {
                let bbox = pg_agg::st_extent_threed(&refs);
                // bbox3d doesn't have a WKB representation directly;
                // format as a BOX3D text per PostGIS convention.
                Ok(SqlValue::Text(format!(
                    "BOX3D({} {} {},{} {} {})",
                    bbox.min_x, bbox.min_y, bbox.min_z, bbox.max_x, bbox.max_y, bbox.max_z
                )))
            }
            AGG_ST_CLUSTER_DBSCAN => {
                // Returns JSON [clusterId or null per input row].
                let eps = state.eps
                    .ok_or_else(|| "st_clusterdbscan_agg: eps never seen".to_string())?;
                let mp = state.min_points
                    .ok_or_else(|| "st_clusterdbscan_agg: min_points never seen".to_string())?;
                let ids = pg_cluster::st_cluster_dbscan(&refs, eps, mp)
                    .map_err(|e| format!("st_clusterdbscan_agg: {}", postgis_err_string(e)))?;
                let json: Vec<String> = ids
                    .iter()
                    .map(|c| match c {
                        Some(n) => format!("{n}"),
                        None => "null".to_string(),
                    })
                    .collect();
                Ok(SqlValue::Text(format!("[{}]", json.join(","))))
            }
            AGG_ST_CLUSTER_KMEANS => {
                let k = state.k
                    .ok_or_else(|| "st_clusterkmeans_agg: k never seen".to_string())?;
                let ids = pg_cluster::st_cluster_kmeans(&refs, k)
                    .map_err(|e| format!("st_clusterkmeans_agg: {}", postgis_err_string(e)))?;
                let json: Vec<String> = ids.iter().map(|n| format!("{n}")).collect();
                Ok(SqlValue::Text(format!("[{}]", json.join(","))))
            }
            other => Err(format!("postgis agg: unknown id {other}")),
        }
    }

    fn value(_func_id: u64, _context_id: u64) -> Result<SqlValue, String> {
        Err("postgis agg: window mode not supported".to_string())
    }

    fn inverse(
        _func_id: u64,
        _context_id: u64,
        _args: Vec<SqlValue>,
    ) -> Result<(), String> {
        Err("postgis agg: window mode not supported".to_string())
    }
}

bindings::export!(PostgisBridge with_types_in bindings);
